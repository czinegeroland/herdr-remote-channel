//! Errors produced by the local state store.

/// Something went wrong reading or writing local state.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    /// The database rejected an operation.
    #[error("local state database error: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The database was written by a newer build.
    ///
    /// Refusing is safer than guessing: this build cannot know what a newer
    /// schema means, and writing to it could corrupt state the newer build
    /// depends on.
    #[error(
        "local state database is at schema version {found}, but this build supports {supported}"
    )]
    SchemaTooNew {
        /// Version found on disk.
        found: u32,
        /// Highest version this build understands.
        supported: u32,
    },

    /// An operation named a channel that is not registered locally.
    #[error("channel {channel_id} is not registered locally")]
    UnknownChannel {
        /// The unknown channel.
        channel_id: String,
    },

    /// An operation named a message that is not in the outbox.
    #[error("message {message_id} is not in the outbox")]
    UnknownMessage {
        /// The unknown message.
        message_id: String,
    },

    /// A protocol derivation failed.
    #[error(transparent)]
    Protocol(#[from] hrc_protocol::ProtocolError),

    /// The same message ID arrived carrying different ciphertext.
    ///
    /// Not a duplicate: a repeat of an at-least-once delivery is identical
    /// by construction, so a differing digest means the object under that
    /// ID was replaced.
    #[error("message {message_id} arrived again with different ciphertext")]
    InboundSubstituted {
        /// The message identifier reused.
        message_id: String,
    },

    /// A sender device's chain branched.
    ///
    /// Either it reused a sequence number or it named a predecessor other
    /// than the one recorded. Both mean two different histories claim the
    /// same position, which is the per-device form of a rewritten log.
    #[error(
        "device {sender_device} has two messages at sequence {device_sequence}: \
         {existing} and {arriving}"
    )]
    InboundForked {
        /// The sender device.
        sender_device: String,
        /// The contested position.
        device_sequence: u64,
        /// What was already recorded there.
        existing: String,
        /// What arrived claiming it.
        arriving: String,
    },

    /// No invite with that identifier is recorded locally.
    #[error("invite {invite_id} is not recorded locally")]
    UnknownInvite {
        /// The unknown invite.
        invite_id: String,
    },

    /// A decision does not name a lifecycle state the inbox understands.
    #[error("decision action `{action}` cannot update an inbox lifecycle")]
    InvalidDecisionAction {
        /// The action that was refused.
        action: String,
    },
}

/// Convenience alias for storage results.
pub type Result<T> = std::result::Result<T, StorageError>;
