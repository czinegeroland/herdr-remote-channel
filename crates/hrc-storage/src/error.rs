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
}

/// Convenience alias for storage results.
pub type Result<T> = std::result::Result<T, StorageError>;
