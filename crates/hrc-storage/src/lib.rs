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

use std::path::Path;

use hrc_protocol::canonical;
use rusqlite::{Connection, OptionalExtension as _, TransactionBehavior, params};

pub use error::{Result, StorageError};

/// A reserved outgoing message slot.
///
/// Returned by [`Database::allocate_outgoing`]. Holding one means the
/// sequence and chain position are durably yours; no other caller can be
/// given them, even after a crash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingReservation {
    /// Sortable message identifier.
    pub message_id: String,
    /// Per-device sequence number.
    pub device_sequence: u64,
    /// Chain ID of this message.
    pub chain_id: String,
    /// Chain ID of the previous message from this device, if any.
    pub previous_chain_id: Option<String>,
    /// Roster epoch the message will be encrypted for.
    pub roster_epoch: u64,
}

/// State of an outbox record.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxState {
    /// Sequence allocated; ciphertext not yet attached.
    Reserved,
    /// Ready to publish.
    Queued,
    /// A publication attempt is in flight.
    Publishing,
    /// Confirmed present in the transport.
    Published,
    /// Abandoned after a crash. The sequence is burned, not reused.
    Gap,
    /// Permanently failed.
    Failed,
}

impl OutboxState {
    /// The stored representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            OutboxState::Reserved => "reserved",
            OutboxState::Queued => "queued",
            OutboxState::Publishing => "publishing",
            OutboxState::Published => "published",
            OutboxState::Gap => "gap",
            OutboxState::Failed => "failed",
        }
    }

    /// Parses a stored representation.
    pub fn parse(value: &str) -> Option<Self> {
        [
            OutboxState::Reserved,
            OutboxState::Queued,
            OutboxState::Publishing,
            OutboxState::Published,
            OutboxState::Gap,
            OutboxState::Failed,
        ]
        .into_iter()
        .find(|state| state.as_str() == value)
    }
}

/// A channel's locally tracked state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRecord {
    /// Channel identifier.
    pub channel_id: String,
    /// Transport kind, for example `git`.
    pub transport_kind: String,
    /// Transport locator.
    pub transport_locator: String,
    /// Local display name. Never sender-controlled.
    pub local_name: String,
    /// Roster epoch evaluated so far.
    pub roster_epoch: u64,
    /// Control sequence applied so far.
    pub control_sequence: u64,
    /// Last fully processed transport revision.
    pub sync_cursor: Option<String>,
    /// Why synchronization is halted, when it is.
    pub halted_reason: Option<String>,
}

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

    /// Registers a channel, or returns an error if it already exists.
    pub fn insert_channel(
        &self,
        channel_id: &str,
        transport_kind: &str,
        transport_locator: &str,
        local_name: &str,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO channel (
                 channel_id, transport_kind, transport_locator, local_name, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                channel_id,
                transport_kind,
                transport_locator,
                local_name,
                now
            ],
        )?;
        Ok(())
    }

    /// Reads a channel's state.
    pub fn channel(&self, channel_id: &str) -> Result<Option<ChannelRecord>> {
        self.connection
            .query_row(
                "SELECT channel_id, transport_kind, transport_locator, local_name,
                        roster_epoch, control_sequence, sync_cursor, halted_reason
                 FROM channel WHERE channel_id = ?1",
                params![channel_id],
                |row| {
                    Ok(ChannelRecord {
                        channel_id: row.get(0)?,
                        transport_kind: row.get(1)?,
                        transport_locator: row.get(2)?,
                        local_name: row.get(3)?,
                        roster_epoch: row.get::<_, i64>(4)? as u64,
                        control_sequence: row.get::<_, i64>(5)? as u64,
                        sync_cursor: row.get(6)?,
                        halted_reason: row.get(7)?,
                    })
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Records progress after a control chain has been evaluated.
    pub fn set_roster_progress(
        &self,
        channel_id: &str,
        roster_epoch: u64,
        control_sequence: u64,
    ) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET roster_epoch = ?2, control_sequence = ?3 WHERE channel_id = ?1",
            params![channel_id, roster_epoch as i64, control_sequence as i64],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Advances the durable synchronization cursor.
    pub fn set_sync_cursor(&self, channel_id: &str, cursor: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET sync_cursor = ?2 WHERE channel_id = ?1",
            params![channel_id, cursor],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Halts synchronization for a channel with a sticky reason.
    ///
    /// The reason is never overwritten by a later halt: the first detected
    /// cause is the one worth showing, and a cascade of downstream failures
    /// should not bury it.
    pub fn halt_channel(&self, channel_id: &str, reason: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET halted_reason = COALESCE(halted_reason, ?2)
             WHERE channel_id = ?1",
            params![channel_id, reason],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Reserves the next outgoing message slot for a device.
    ///
    /// Everything PRD section 17.2 requires to be allocated together is
    /// allocated in one transaction: the message ID, the device sequence,
    /// the predecessor chain ID, the payload hash, and the outbox record.
    ///
    /// The chain ID is derived exactly as PRD section 18.1 specifies, over
    /// the channel, device, sequence, and message ID — not over ciphertext
    /// or recipients, so a roster change that forces re-encryption does not
    /// invalidate the links of messages already queued behind it.
    pub fn allocate_outgoing(
        &mut self,
        channel_id: &str,
        device_id: &str,
        message_id: &str,
        roster_epoch: u64,
        payload_hash: &str,
        now: &str,
    ) -> Result<OutgoingReservation> {
        // IMMEDIATE, not the default DEFERRED. A deferred transaction takes
        // a read lock first and upgrades on the first write; SQLite refuses
        // that upgrade with SQLITE_BUSY *without* consulting the busy
        // timeout, because waiting could deadlock two upgraders. Taking the
        // write lock up front makes concurrent allocation wait its turn
        // instead of failing, which PRD section 17.2 requires of concurrent
        // local callers.
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        let exists: bool = transaction
            .query_row(
                "SELECT 1 FROM channel WHERE channel_id = ?1",
                params![channel_id],
                |_| Ok(true),
            )
            .optional()?
            .unwrap_or(false);
        if !exists {
            return Err(StorageError::UnknownChannel {
                channel_id: channel_id.to_owned(),
            });
        }

        // Take the write lock on the sequence row and read the previous
        // chain ID in the same statement, so no other writer can interleave.
        transaction.execute(
            "INSERT INTO device_sequence (channel_id, device_id, last_sequence, last_chain_id)
             VALUES (?1, ?2, 0, NULL)
             ON CONFLICT (channel_id, device_id) DO NOTHING",
            params![channel_id, device_id],
        )?;

        let (device_sequence, previous_chain_id) = transaction.query_row(
            "UPDATE device_sequence SET last_sequence = last_sequence + 1
             WHERE channel_id = ?1 AND device_id = ?2
             RETURNING last_sequence, last_chain_id",
            params![channel_id, device_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)? as u64,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )?;

        let chain_id = chain_id_for(channel_id, device_id, device_sequence, message_id)?;

        transaction.execute(
            "UPDATE device_sequence SET last_chain_id = ?3
             WHERE channel_id = ?1 AND device_id = ?2",
            params![channel_id, device_id, chain_id],
        )?;

        transaction.execute(
            "INSERT INTO outbox (
                 message_id, channel_id, device_id, device_sequence, chain_id,
                 previous_chain_id, roster_epoch, payload_hash, state, created_at, updated_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'reserved', ?9, ?9)",
            params![
                message_id,
                channel_id,
                device_id,
                device_sequence as i64,
                chain_id,
                previous_chain_id,
                roster_epoch as i64,
                payload_hash,
                now,
            ],
        )?;

        transaction.commit()?;

        Ok(OutgoingReservation {
            message_id: message_id.to_owned(),
            device_sequence,
            chain_id,
            previous_chain_id,
            roster_epoch,
        })
    }

    /// Attaches ciphertext to a reserved slot and queues it for publication.
    pub fn queue_outgoing(&self, message_id: &str, ciphertext: &[u8], now: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE outbox SET ciphertext = ?2, state = 'queued', updated_at = ?3
             WHERE message_id = ?1 AND state IN ('reserved', 'queued')",
            params![message_id, ciphertext, now],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Marks a queued message as published.
    pub fn mark_published(&self, message_id: &str, now: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE outbox SET state = 'published', updated_at = ?2, last_error = NULL
             WHERE message_id = ?1",
            params![message_id, now],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Records a failed publication attempt without losing the message.
    pub fn record_attempt_failure(&self, message_id: &str, error: &str, now: &str) -> Result<u64> {
        let attempts = self
            .connection
            .query_row(
                "UPDATE outbox SET attempts = attempts + 1, last_error = ?2, updated_at = ?3
                 WHERE message_id = ?1
                 RETURNING attempts",
                params![message_id, error, now],
                |row| row.get::<_, i64>(0),
            )
            .optional()?
            .ok_or_else(|| StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            })?;

        Ok(attempts as u64)
    }

    /// Messages awaiting publication, in send order.
    pub fn pending_outgoing(&self, channel_id: &str) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id FROM outbox
             WHERE channel_id = ?1 AND state IN ('queued', 'publishing')
             ORDER BY device_sequence",
        )?;
        let rows = statement.query_map(params![channel_id], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Resolves reservations left behind by a crash.
    ///
    /// A reserved slot has a sequence but no ciphertext, so nothing can be
    /// published for it. PRD section 17.2 allows either publishing the
    /// reserved record or marking the sequence an explicit local gap.
    /// Reusing the sequence is not an option: a peer may already have seen a
    /// message claiming it.
    pub fn recover_reservations(&self, channel_id: &str, now: &str) -> Result<u64> {
        let marked = self.connection.execute(
            "UPDATE outbox SET state = 'gap', updated_at = ?2
             WHERE channel_id = ?1 AND state = 'reserved' AND ciphertext IS NULL",
            params![channel_id, now],
        )?;
        Ok(marked as u64)
    }

    /// The state of one outbox record.
    pub fn outbox_state(&self, message_id: &str) -> Result<Option<OutboxState>> {
        let stored: Option<String> = self
            .connection
            .query_row(
                "SELECT state FROM outbox WHERE message_id = ?1",
                params![message_id],
                |row| row.get(0),
            )
            .optional()?;

        Ok(stored.as_deref().and_then(OutboxState::parse))
    }

    /// Appends an audit record. There is no update or delete counterpart.
    pub fn append_audit(
        &self,
        channel_id: Option<&str>,
        message_id: Option<&str>,
        action: &str,
        content_hash: Option<&str>,
        detail: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO audit (channel_id, message_id, action, content_hash, detail, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![channel_id, message_id, action, content_hash, detail, now],
        )?;
        Ok(())
    }

    /// Audit actions in the order they occurred.
    pub fn audit_actions(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT action FROM audit ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
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

/// Derives the per-device message chain ID (PRD section 18.1).
pub fn chain_id_for(
    channel_id: &str,
    device_id: &str,
    device_sequence: u64,
    message_id: &str,
) -> Result<String> {
    // Serialized through the protocol crate so the derivation uses the same
    // canonical encoding as everything else that is hashed or signed.
    let value = serde_json::json!({
        "domain": hrc_protocol::domain::MESSAGE_CHAIN,
        "channelId": channel_id,
        "senderDeviceId": device_id,
        "deviceSequence": device_sequence,
        "messageId": message_id,
    });

    Ok(canonical::canonical_sha256_hex(&value)?)
}

#[cfg(test)]
mod tests;
