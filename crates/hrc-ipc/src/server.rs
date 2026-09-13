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
    let listener = bind(endpoint)?;
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

/// Binds one endpoint, replacing a stale socket file if one is in the way.
fn bind(endpoint: &Endpoint) -> Result<interprocess::local_socket::tokio::Listener> {
    let name = resolve(endpoint)?;

    let options = ListenerOptions::new().name(name.clone());

    match options.create_tokio() {
        Ok(listener) => Ok(listener),
        Err(error) if error.kind() == std::io::ErrorKind::AddrInUse => {
            // Either a live daemon or a socket file left behind by one that
            // was killed. Removing and retrying distinguishes them: a live
            // daemon still holds the name and the retry fails.
            if let Some(path) = endpoint.path() {
                std::fs::remove_file(&path)?;
                let retry = ListenerOptions::new().name(name);
                let listener = retry.create_tokio()?;
                restrict(endpoint)?;
                return Ok(listener);
            }
            Err(error.into())
        }
        Err(error) => Err(error.into()),
    }
    .inspect(|_| {
        let _ = restrict(endpoint);
    })
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
