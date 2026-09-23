//! Durable local state for Herdr Remote Channel.
//!
//! Owns the SQLite database holding channel state, the outbox, the
//! quarantined inbox, the audit log, and synchronization cursors
//! (PRD sections 12.5 and 17.2).
//!
//! Two properties drive the design:
//!
//! - **Nothing secret lives here.** Private keys stay in the OS keychain
//!   (PRD section 14.1). This database holds ciphertext, metadata, and
//!   quarantined plaintext that a human has already been shown or will be
//!   shown through the trusted surface.
//! - **Allocation is one transaction.** PRD section 17.2 requires the
//!   message ID, device sequence, predecessor chain ID, payload hash, and
//!   outbox record to be reserved together. Two concurrent callers must
//!   never receive the same sequence, and a crash between reservation and
//!   publication must leave a record that recovery can resolve rather than a
//!   silent hole.

pub mod error;
pub mod schema;

use std::collections::BTreeMap;
use std::path::Path;

use hrc_protocol::canonical;
use hrc_protocol::message::RecipientPredecessors;
use rusqlite::{Connection, OptionalExtension as _, Transaction, TransactionBehavior, params};

pub use error::{Result, StorageError};

mod channel;
mod context;
mod decisions;
mod inbox;
mod invites;
mod local;
mod ordering;
mod outbox;
mod receipts;

pub use channel::*;
pub use context::*;
pub use decisions::*;
pub use inbox::*;
pub use invites::*;
pub use outbox::*;
pub use receipts::*;

/// The local state database.
///
/// `Debug` prints the schema version only. The connection handle would
/// otherwise be noise, and nothing about the stored rows belongs in a log
/// line by accident.
pub struct Database {
    connection: Connection,
}

impl std::fmt::Debug for Database {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Database")
            .field("schema_version", &schema::SCHEMA_VERSION)
            .finish_non_exhaustive()
    }
}

pub(crate) fn ensure_channel(transaction: &Transaction<'_>, channel_id: &str) -> Result<()> {
    let exists = transaction
        .query_row(
            "SELECT 1 FROM channel WHERE channel_id = ?1",
            params![channel_id],
            |_| Ok(true),
        )
        .optional()?
        .unwrap_or(false);
    if exists {
        Ok(())
    } else {
        Err(StorageError::UnknownChannel {
            channel_id: channel_id.to_owned(),
        })
    }
}

impl Database {
    /// Opens or creates the database at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self> {
        let connection = Connection::open(path)?;
        Self::configure(connection)
    }

    /// Opens a private in-memory database, for tests.
    pub fn open_in_memory() -> Result<Self> {
        let connection = Connection::open_in_memory()?;
        Self::configure(connection)
    }

    /// Applies the pragmas and migrations every connection needs.
    fn configure(connection: Connection) -> Result<Self> {
        // WAL keeps readers from blocking the daemon's writes (PRD
        // requirement HRC-TECH-007). It is persistent, so setting it on an
        // existing database is a no-op.
        connection.pragma_update(None, "journal_mode", "WAL")?;
        // NORMAL is the documented safe pairing with WAL: a crash cannot
        // corrupt the database, though the last transactions may be lost.
        // Anything the outbox promises is committed before it is promised.
        connection.pragma_update(None, "synchronous", "NORMAL")?;
        connection.pragma_update(None, "foreign_keys", true)?;
        // Concurrent CLI processes contend with the daemon for the write
        // lock; wait rather than failing the user's command outright.
        connection.busy_timeout(std::time::Duration::from_secs(5))?;

        let mut database = Self { connection };
        schema::migrate(&mut database.connection)?;
        Ok(database)
    }

    /// Runs SQLite's own integrity check, for `hrc doctor`.
    pub fn integrity_check(&self) -> Result<bool> {
        let result: String = self
            .connection
            .query_row("PRAGMA integrity_check", [], |row| row.get(0))?;

        Ok(result == "ok")
    }

    /// Current UTC time from SQLite, in RFC 3339 form.
    pub fn utc_now(&self) -> Result<String> {
        self.connection
            .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |row| {
                row.get(0)
            })
            .map_err(StorageError::from)
    }

    /// Fails when an update matched no channel row.
    fn expect_one(updated: usize, channel_id: &str) -> Result<()> {
        if updated == 0 {
            Err(StorageError::UnknownChannel {
                channel_id: channel_id.to_owned(),
            })
        } else {
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests;
