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
        }
    }

    /// The documented exit code for this failure (PRD section 22.8).
    pub fn exit_code(&self) -> i32 {
        match self {
            // A missing passphrase or an uninitialized installation is the
            // user's command being wrong for the current state, not a
            // runtime fault.
            CliError::NoStateDirectory | CliError::NoPassphrase | CliError::AlreadyInitialized => {
                exit::USAGE
            }
            CliError::Io { .. } | CliError::Storage(_) | CliError::Crypto(_) => exit::FAILURE,
        }
    }
}

/// Convenience alias for command results.
pub type Result<T> = std::result::Result<T, CliError>;
