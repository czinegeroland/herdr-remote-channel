//! Errors surfaced by the command line.
//!
//! Each variant carries the exit code it maps to, so the mapping lives with
//! the error rather than in a match at the call site that could drift from
//! the documented contract in PRD section 22.8.

use crate::exit;

/// Something went wrong running a command.
#[derive(Debug, thiserror::Error)]
pub enum CliError {
    /// No state directory could be determined.
    #[error("could not determine where to keep local state; set HRC_HOME to a directory")]
    NoStateDirectory,

    /// The key store passphrase was not supplied.
    #[error("no key store passphrase; set HRC_PASSPHRASE for non-interactive use")]
    NoPassphrase,

    /// This installation already has device keys.
    ///
    /// Overwriting them would discard the identity every channel knows this
    /// installation by, so it is refused rather than confirmed.
    #[error("this installation is already initialized; remove its key first to start over")]
    AlreadyInitialized,

    /// A filesystem operation failed.
    #[error("could not {action}: {source}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// Local state could not be read or written.
    #[error(transparent)]
    Storage(#[from] hrc_storage::StorageError),

    /// A key or cryptographic operation failed.
    #[error(transparent)]
    Crypto(#[from] hrc_crypto::CryptoError),

    /// The synchronization core refused an operation.
    #[error(transparent)]
    Core(#[from] hrc_core::CoreError),

    /// The Git transport failed.
    #[error(transparent)]
    Git(#[from] hrc_transport_git::GitError),

    /// Local IPC failed.
    #[error(transparent)]
    Ipc(#[from] hrc_ipc::IpcError),

    /// A protocol object could not be built or validated.
    #[error(transparent)]
    Protocol(#[from] hrc_protocol::ProtocolError),

    /// The transport refused an operation.
    #[error(transparent)]
    Transport(#[from] hrc_transport::TransportError),

    /// This installation already has local state for that channel.
    #[error("channel {channel_id} already exists locally")]
    ChannelExists {
        /// The channel already registered.
        channel_id: String,
    },

    /// The operating system would not supply randomness.
    #[error("could not obtain randomness from the operating system")]
    Entropy,
}

impl CliError {
    /// A stable machine-readable code for this failure.
    pub fn code(&self) -> &'static str {
        match self {
            CliError::NoStateDirectory => "no_state_directory",
            CliError::NoPassphrase => "no_passphrase",
            CliError::AlreadyInitialized => "already_initialized",
            CliError::Io { .. } => "io_error",
            CliError::Storage(_) => "storage_error",
            CliError::Crypto(_) => "crypto_error",
            CliError::Core(_) => "core_error",
            CliError::Git(_) => "git_error",
            CliError::Ipc(_) => "ipc_error",
            CliError::Protocol(_) => "protocol_error",
            CliError::Transport(_) => "transport_error",
            CliError::ChannelExists { .. } => "channel_exists",
            CliError::Entropy => "entropy_unavailable",
        }
    }

    /// The documented exit code for this failure (PRD section 22.8).
    pub fn exit_code(&self) -> i32 {
        match self {
            // A missing passphrase or an uninitialized installation is the
            // user's command being wrong for the current state, not a
            // runtime fault.
            CliError::NoStateDirectory
            | CliError::NoPassphrase
            | CliError::AlreadyInitialized
            | CliError::ChannelExists { .. } => exit::USAGE,
            CliError::Io { .. }
            | CliError::Storage(_)
            | CliError::Crypto(_)
            | CliError::Core(_)
            | CliError::Git(_)
            | CliError::Ipc(_)
            | CliError::Protocol(_)
            | CliError::Transport(_)
            | CliError::Entropy => exit::FAILURE,
        }
    }
}

/// Convenience alias for command results.
pub type Result<T> = std::result::Result<T, CliError>;
