//! Framed local IPC for the daemon's two interfaces.
//!
//! PRD requirement HRC-TECH-009: one abstraction over Unix-domain sockets and
//! Windows named pipes. The daemon listens on two of these — the agent-safe
//! interface and the trusted human interface of PRD section 19.5 — and the
//! separation between them is the *endpoint*, not a field in any message. A
//! caller's authority is decided by which socket accepted it, because
//! anything carried inside a request is something the caller chose.
//!
//! The wire format is deliberately dull: a four-byte big-endian length
//! followed by that many bytes of JSON. Length prefixing is what makes a
//! partial read unambiguous, and checking the length against a maximum before
//! allocating is what stops a local process from asking the daemon to reserve
//! four gigabytes.
//!
//! This crate knows nothing about what the messages mean. It moves bytes and
//! enforces the frame; deciding what an agent-safe caller may ask for belongs
//! to `hrc-core`.

#![warn(missing_docs)]

pub mod endpoint;
pub mod frame;

mod client;
mod server;

pub use client::Client;
pub use endpoint::Endpoint;
pub use frame::{MAX_FRAME_BYTES, read_frame, write_frame};
pub use server::{Handler, serve};

/// Something went wrong moving a message between two local processes.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The peer closed the connection, cleanly or otherwise.
    #[error("the peer closed the connection")]
    Disconnected,

    /// A frame declared a length above [`MAX_FRAME_BYTES`].
    ///
    /// Reported before anything is allocated: the point of the limit is that
    /// the daemon never reserves memory a caller asked it to.
    #[error("frame of {declared} bytes exceeds the {limit} byte limit")]
    FrameTooLarge {
        /// The length the frame declared.
        declared: usize,
        /// The maximum accepted.
        limit: usize,
    },

    /// The frame body was not valid JSON, or not the expected shape.
    #[error("malformed frame: {0}")]
    Malformed(String),

    /// The endpoint name could not be built for this platform.
    #[error("invalid endpoint name: {0}")]
    InvalidEndpoint(String),

    /// The underlying socket or pipe failed.
    #[error("ipc transport: {0}")]
    Io(#[from] std::io::Error),
}

/// Result alias for this crate.
pub type Result<T> = std::result::Result<T, IpcError>;
