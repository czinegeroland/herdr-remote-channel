//! Accepting connections on one endpoint.
//!
//! One listener serves one interface. That is the whole authority model: a
//! handler installed on the agent-safe endpoint never sees a trusted request
//! because a trusted client is not connected to it, and no code path lets a
//! connection change which handler it reaches.

use std::sync::Arc;

use interprocess::local_socket::ListenerOptions;
use interprocess::local_socket::tokio::prelude::*;
#[cfg(not(windows))]
use interprocess::local_socket::{GenericFilePath, ToFsName};
#[cfg(windows)]
use interprocess::local_socket::{GenericNamespaced, ToNsName};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::AsyncWriteExt as _;

use crate::endpoint::Endpoint;
use crate::frame::{read_frame, write_frame};
use crate::{IpcError, Result};

/// Answers requests arriving on one endpoint.
///
/// Implementations are shared across connections, so state behind them needs
/// its own synchronization. Returning an error ends that one connection
/// rather than the listener: a client that sent nonsense must not be able to
/// stop the daemon serving everyone else.
pub trait Handler<Request, Response>: Send + Sync + 'static {
    /// Produces the answer to one request.
    fn handle(&self, request: Request) -> Response;
}

impl<F, Request, Response> Handler<Request, Response> for F
where
    F: Fn(Request) -> Response + Send + Sync + 'static,
{
    fn handle(&self, request: Request) -> Response {
        self(request)
    }
}

/// Binds `endpoint` and serves connections until the future is dropped.
///
/// A stale socket file from a daemon that did not shut down cleanly is
/// removed first. That is safe only because binding is what proves nobody is
/// listening: if a live daemon holds the endpoint, the bind below fails and
/// this returns rather than stealing the name.
pub async fn serve<Request, Response, H>(endpoint: &Endpoint, handler: H) -> Result<()>
where
    Request: DeserializeOwned + Send + 'static,
    Response: Serialize + Send + Sync + 'static,
    H: Handler<Request, Response>,
{
    let listener = bind(endpoint).await?;
    let handler = Arc::new(handler);

    loop {
        let stream = match listener.accept().await {
            Ok(stream) => stream,
            // One failed accept is not a reason to stop serving.
            Err(error) if is_recoverable(&error) => continue,
            Err(error) => return Err(error.into()),
        };

        let handler = Arc::clone(&handler);
        tokio::spawn(async move {
            let _ = serve_connection::<Request, Response, H>(stream, handler).await;
        });
    }
}

/// How long to keep trying to take an endpoint whose name is still in use.
///
/// A restarted daemon races its predecessor's teardown. On Unix a socket file
/// the previous process abandoned can be removed, but on Windows a pipe name
/// cannot: the only option is to wait for the old instance to go away.
/// Bounded, so a *live* daemon still produces an error rather than a hang.
const BIND_RETRY_BUDGET: std::time::Duration = std::time::Duration::from_secs(2);

/// The pause between bind attempts.
const BIND_RETRY_DELAY: std::time::Duration = std::time::Duration::from_millis(25);

/// Binds one endpoint, waiting out a predecessor that has not finished
/// releasing the name.
async fn bind(endpoint: &Endpoint) -> Result<interprocess::local_socket::tokio::Listener> {
    let deadline = std::time::Instant::now() + BIND_RETRY_BUDGET;

    loop {
        match try_bind(endpoint) {
            Ok(listener) => {
                // Best effort: where the endpoint is not a file there is
                // nothing to restrict, and the name is already scoped to the
                // user's session.
                let _ = restrict(endpoint);
                return Ok(listener);
            }
            // Retry only while the name might still clear. Something that
            // answers is a live daemon, and waiting for it would turn a
            // definite conflict into a pause before the same error.
            Err(error)
                if is_in_use(&error)
                    && !anyone_answering(endpoint)
                    && std::time::Instant::now() < deadline =>
            {
                tokio::time::sleep(BIND_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

/// One bind attempt, clearing a socket file its owner abandoned.
fn try_bind(endpoint: &Endpoint) -> Result<interprocess::local_socket::tokio::Listener> {
    let name = resolve(endpoint)?;

    match ListenerOptions::new().name(name.clone()).create_tokio() {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            // Either a live daemon or a socket file left behind by one that
            // was killed. Probe before removing: unlinking a live daemon's
            // socket and binding a new one at the same path *succeeds*, and
            // leaves that daemon listening on a socket no client can reach
            // any more. The name being taken is not proof that anyone is
            // still answering on it.
            //
            // On Windows there is no file to remove, so the caller's retry
            // budget is what covers a restart.
            match endpoint.path() {
                Some(path) if !live_listener(&path)? => {
                    std::fs::remove_file(&path)?;
                    Ok(ListenerOptions::new().name(name).create_tokio()?)
                }
                _ => Err(error.into()),
            }
        }
        Err(error) => Err(error.into()),
    }
}

/// Whether something is still accepting connections at a socket path.
#[cfg(unix)]
fn live_listener(path: &std::path::Path) -> Result<bool> {
    match std::os::unix::net::UnixStream::connect(path) {
        Ok(_) => Ok(true),
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::NotFound
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error.into()),
    }
}

/// Never reached where an endpoint is not a filesystem path, but it has to
/// exist for the call above to compile.
#[cfg(not(unix))]
fn live_listener(_path: &std::path::Path) -> Result<bool> {
    Ok(true)
}

/// Whether a live listener is answering on this endpoint.
///
/// On Unix this is knowable, so a conflict with a running daemon is reported
/// at once. On Windows a pipe name in use tells us nothing about whether its
/// owner is still there, so this says no and the retry budget decides — the
/// wait is what covers a restart, which is the common case.
fn anyone_answering(endpoint: &Endpoint) -> bool {
    match endpoint.path() {
        Some(path) => live_listener(&path).unwrap_or(false),
        None => false,
    }
}

/// Whether the endpoint name is still held by someone else.
fn is_in_use(error: &IpcError) -> bool {
    matches!(
        error,
        IpcError::Io(io)
            if matches!(
                io.kind(),
                std::io::ErrorKind::AddrInUse | std::io::ErrorKind::PermissionDenied
            )
    )
}

/// Sets owner-only permissions on the socket file where that applies.
fn restrict(endpoint: &Endpoint) -> Result<()> {
    #[cfg(unix)]
    if let Some(path) = endpoint.path() {
        use std::os::unix::fs::PermissionsExt as _;

        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    }

    #[cfg(not(unix))]
    let _ = endpoint;

    Ok(())
}

/// Whether an accept error is worth continuing after.
fn is_recoverable(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::ConnectionAborted
            | std::io::ErrorKind::ConnectionReset
            | std::io::ErrorKind::Interrupted
            | std::io::ErrorKind::WouldBlock
    )
}

/// Reads and answers frames until the peer disconnects.
async fn serve_connection<Request, Response, H>(
    stream: interprocess::local_socket::tokio::Stream,
    handler: Arc<H>,
) -> Result<()>
where
    Request: DeserializeOwned,
    Response: Serialize,
    H: Handler<Request, Response>,
{
    let (mut reader, mut writer) = stream.split();

    loop {
        match read_frame::<_, Request>(&mut reader).await {
            Ok(request) => {
                let response = handler.handle(request);
                write_frame(&mut writer, &response).await?;
            }
            Err(IpcError::Disconnected) => {
                let _ = writer.shutdown().await;
                return Ok(());
            }
            // A malformed or oversized frame desynchronizes the stream:
            // there is no way to know where the next frame starts, so the
            // only correct response is to drop this connection.
            Err(error) => {
                let _ = writer.shutdown().await;
                return Err(error);
            }
        }
    }
}

/// Resolves an endpoint to the platform's socket name type.
///
/// Split out so the two call sites cannot drift: a client that resolved a
/// name differently from the listener would simply fail to find it.
fn resolve(endpoint: &Endpoint) -> Result<interprocess::local_socket::Name<'_>> {
    #[cfg(windows)]
    {
        Ok(endpoint.name().to_ns_name::<GenericNamespaced>()?)
    }

    #[cfg(not(windows))]
    {
        Ok(endpoint.name().to_fs_name::<GenericFilePath>()?)
    }
}
