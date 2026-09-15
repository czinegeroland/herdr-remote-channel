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

    /// This installation has no channel configured.
    #[error("no channel is configured; run `hrc create` or `hrc join` first")]
    NoChannel,

    /// The trusted approval screen was asked for without a human present.
    ///
    /// PRD section 22.7 refuses *non-interactive* invocation, which is the
    /// distinction that matters: the boundary is not that a human may never
    /// approve from a terminal, it is that a program may not. A pipe, a
    /// captured subprocess, and an agent's tool call are all indistinguishable
    /// from each other here, and none of them is a person.
    #[error(
        "this decision needs an interactive terminal; open the matching \
         Herdr pane (`Remote channel review` for messages, `Remote channel \
         join requests` for members), or run the command directly in a \
         terminal"
    )]
    NotInteractive,

    /// A context send was reached without the preview that authorizes it.
    ///
    /// Section 22.4 binds the authorization to the package digest, the
    /// recipient, the channel and the action, and trusted send must present
    /// one. Reaching a send without it means the screen and the daemon
    /// disagree about what was previewed, which is a bug rather than
    /// something to retry.
    #[error("no context preview authorized this send; preview the package again")]
    NoContextAuthorization,

    /// The daemon's trusted interface did not answer.
    ///
    /// Every disclosure of a quarantined body goes through the daemon, so
    /// there is no degraded mode to fall back to here: without it the screen
    /// would have nothing to show.
    #[error("the hrc daemon is not running; start it with `hrc daemon`")]
    DaemonUnavailable,

    /// The confirmation was recorded but the visibility change could not be
    /// made from here.
    #[error("{reason}")]
    PublicationUnavailable {
        /// What went wrong and what the human should do instead.
        reason: String,
    },

    /// A Herdr manifest named an action or pane this build does not have.
    #[error("`{name}` is not a Herdr {kind} this build provides")]
    UnknownHerdrTarget {
        /// Whether an action or a pane was named.
        kind: &'static str,
        /// The name the manifest used.
        name: String,
    },

    /// Several channels exist and the command did not say which.
    #[error("several channels are configured; pass --channel to choose one")]
    AmbiguousChannel,

    /// The channel has no published control log to replay.
    #[error("channel {channel_id} has no published control log")]
    ChannelNotPublished {
        /// The channel in question.
        channel_id: String,
    },

    /// This installation's device is not in the channel's roster.
    #[error("this device is not a member of the channel")]
    LocalDeviceNotInChannel,

    /// The invite lapsed before it was redeemed.
    #[error("this invite expired at {expires_at}")]
    InviteExpired {
        /// When it lapsed.
        expires_at: String,
    },

    /// A lifetime such as `24h` could not be understood.
    #[error("`{value}` is not a lifetime like `30m`, `24h`, or `7d`")]
    InvalidLifetime {
        /// What was supplied.
        value: String,
    },

    /// No message with that identifier is known locally.
    #[error("no message {message_id} in this channel")]
    NoSuchMessage {
        /// The message asked about.
        message_id: String,
    },

    /// The invite names a different channel than the repository holds.
    #[error("the invite is for channel {expected}, but the repository holds {found}")]
    InviteChannelMismatch {
        /// The channel the invite named.
        expected: String,
        /// The channel the repository actually holds.
        found: String,
    },

    /// A context package with file excerpts needs its source repository.
    #[error("context package {package_id} has excerpts but no repository was supplied")]
    ContextRepositoryRequired {
        /// The package that cannot be checked.
        package_id: String,
    },

    /// A source file or line range cannot form a safe excerpt.
    #[error("cannot use this context source: {reason}")]
    InvalidContextSource {
        /// The safe diagnostic; never source bytes.
        reason: &'static str,
    },
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
            CliError::NoChannel => "no_channel",
            // Deliberately the same stable code as the boundary refusal.
            // PRD section 22.7 requires non-interactive invocation of a
            // trusted operation to fail with *the* authorization-required
            // error, and a caller scripting `hrc review` must not be able to
            // tell "no human here" apart from "not allowed" — both mean the
            // same thing to a program, and a second code would invite
            // retrying against the first.
            CliError::NotInteractive => "authorization_required",
            CliError::DaemonUnavailable => "daemon_unavailable",
            CliError::NoContextAuthorization => "no_context_authorization",
            CliError::AmbiguousChannel => "ambiguous_channel",
            CliError::PublicationUnavailable { .. } => "publication_unavailable",
            CliError::UnknownHerdrTarget { .. } => "unknown_herdr_target",
            CliError::ChannelNotPublished { .. } => "channel_not_published",
            CliError::LocalDeviceNotInChannel => "device_not_in_channel",
            CliError::InviteExpired { .. } => "invite_expired",
            CliError::InvalidLifetime { .. } => "invalid_lifetime",
            CliError::NoSuchMessage { .. } => "no_such_message",
            CliError::InviteChannelMismatch { .. } => "invite_channel_mismatch",
            CliError::ContextRepositoryRequired { .. } => "context_repository_required",
            CliError::InvalidContextSource { .. } => "invalid_context_source",
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
            | CliError::ChannelExists { .. }
            | CliError::NoChannel
            | CliError::AmbiguousChannel
            | CliError::InviteExpired { .. }
            | CliError::InvalidLifetime { .. }
            | CliError::NoSuchMessage { .. }
            | CliError::ContextRepositoryRequired { .. }
            | CliError::InvalidContextSource { .. }
            | CliError::UnknownHerdrTarget { .. } => exit::USAGE,
            CliError::Io { .. }
            | CliError::Storage(_)
            | CliError::Crypto(_)
            | CliError::Core(_)
            | CliError::Git(_)
            | CliError::Ipc(_)
            | CliError::Protocol(_)
            | CliError::Transport(_)
            | CliError::Entropy
            | CliError::ChannelNotPublished { .. }
            | CliError::LocalDeviceNotInChannel
            | CliError::InviteChannelMismatch { .. }
            | CliError::DaemonUnavailable
            | CliError::NoContextAuthorization
            | CliError::PublicationUnavailable { .. } => exit::FAILURE,

            // Same reasoning as the code above: to a program this is the
            // authorization boundary refusing, and it must exit like one.
            CliError::NotInteractive => exit::AUTHORIZATION_REQUIRED,
        }
    }
}

/// Convenience alias for command results.
pub type Result<T> = std::result::Result<T, CliError>;
