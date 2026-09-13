//! Connecting to one endpoint.
//!
//! A client is one connection carrying one request at a time. Requests are
//! not pipelined, because a response frame carries no correlation ID and
//! adding one would be inventing a protocol the daemon does not need: the
//! CLI, the TUI, and an agent each ask a question and wait for its answer.

use std::time::Duration;

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

/// One connection to one of the daemon's interfaces.
#[derive(Debug)]
pub struct Client {
    stream: interprocess::local_socket::tokio::Stream,
}

impl Client {
    /// Connects to `endpoint`.
    pub async fn connect(endpoint: &Endpoint) -> Result<Self> {
        Ok(Self {
            stream: interprocess::local_socket::tokio::Stream::connect(resolve(endpoint)?).await?,
        })
    }

    /// Connects, retrying while the daemon is still coming up.
    ///
    /// A daemon that was just started, or restarted, is briefly not
    /// listening. Retrying a bounded number of times turns that race into a
    /// short wait rather than an error the caller has to interpret. Failures
    /// that are not "nothing is listening there" are returned immediately:
    /// retrying a malformed endpoint name would only delay the same answer.
    pub async fn connect_with_retry(
        endpoint: &Endpoint,
        attempts: u32,
        delay: Duration,
    ) -> Result<Self> {
        let mut last = None;

        for attempt in 0..attempts.max(1) {
            match Self::connect(endpoint).await {
                Ok(client) => return Ok(client),
                Err(IpcError::Io(error)) if is_not_listening(&error) => {
                    last = Some(IpcError::Io(error));
                    if attempt + 1 < attempts {
                        tokio::time::sleep(delay).await;
                    }
                }
                Err(error) => return Err(error),
            }
        }

        Err(last.unwrap_or(IpcError::Disconnected))
    }

    /// Sends one request and reads its answer.
    pub async fn call<Request, Response>(&mut self, request: &Request) -> Result<Response>
    where
        Request: Serialize + ?Sized,
        Response: DeserializeOwned,
    {
        write_frame(&mut self.stream, request).await?;
        read_frame(&mut self.stream).await
    }

    /// Closes the connection.
    pub async fn close(mut self) -> Result<()> {
        self.stream.shutdown().await?;
        Ok(())
    }
}

/// Whether the error means "nothing is listening yet" rather than a real fault.
fn is_not_listening(error: &std::io::Error) -> bool {
    matches!(
        error.kind(),
        std::io::ErrorKind::NotFound
            | std::io::ErrorKind::ConnectionRefused
            | std::io::ErrorKind::AddrNotAvailable
            | std::io::ErrorKind::WouldBlock
    )
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
