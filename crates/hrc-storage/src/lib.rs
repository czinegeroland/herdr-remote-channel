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
    /// Last durable chain link addressed to each recipient device.
    pub recipient_previous_chain_ids: RecipientPredecessors,
}

/// Ciphertext and protected logical material produced inside an outbox transaction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutgoingBuild {
    /// Ciphertext ready for transport publication.
    pub ciphertext: Vec<u8>,
    /// Logical envelope encrypted only to the local sending device.
    pub reseal_material: Vec<u8>,
}

/// One queued outbox entry ready for publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingOutgoing {
    /// Sortable message identifier.
    pub message_id: String,
    /// Sending device whose sequence and chain this message occupies.
    pub device_id: String,
    /// Position in the sending device's chain.
    pub device_sequence: u64,
    /// Chain ID derived from the stable message identity fields.
    pub chain_id: String,
    /// Chain ID of the preceding message from this device, if any.
    pub previous_chain_id: Option<String>,
    /// Roster epoch the current ciphertext was sealed for.
    pub roster_epoch: u64,
    /// Hash of the logical body, stable across re-encryption.
    pub payload_hash: String,
    /// RFC 3339 UTC creation time used for the message object path.
    pub created_at: String,
    /// Stored ciphertext bytes.
    pub ciphertext: Vec<u8>,
    /// Locally encrypted logical envelope used only when re-encryption is required.
    pub reseal_material: Option<Vec<u8>>,
    /// Whether an earlier recipient-set change invalidated this message's map.
    pub recipient_order_stale: bool,
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
    /// Last transport revision whose message bodies were received locally.
    pub receive_cursor: Option<String>,
    /// Why synchronization is halted, when it is.
    pub halted_reason: Option<String>,
}

/// Counts reported by `hrc status` for one channel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelCounts {
    /// Messages awaiting publication.
    pub outbox_pending: u64,
    /// Inbox entries not yet read.
    pub unread: u64,
    /// Inbox entries awaiting a local approval decision.
    pub pending_approval: u64,
    /// The most recent transport error recorded against this channel.
    pub last_error: Option<String>,
}

/// What an arriving message is, relative to what has already been accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboundOutcome {
    /// Accepted as the next message from this sender device.
    Accepted {
        /// Local arrival order, used for display instead of `created_at`.
        arrival_sequence: u64,
        /// Messages that were waiting on this one and are now accepted too,
        /// in the order they were released.
        released: Vec<String>,
    },
    /// A receipt advanced the chain without becoming an inbox entry.
    ReceiptAccepted {
        /// Human messages released by this receipt, in chain order.
        released: Vec<String>,
    },
    /// An authenticated expired message advanced ordering without becoming actionable.
    ExpiredAccepted {
        /// Non-expired human messages released by this link, in chain order.
        released: Vec<String>,
    },
    /// Already accepted, byte for byte. At-least-once delivery is normal.
    Duplicate,
    /// Held: its predecessor from this sender device has not arrived.
    Held {
        /// The chain link being waited for.
        waiting_for: String,
    },
}

/// One message arriving from the transport.
#[derive(Debug, Clone)]
pub struct InboundMessage<'a> {
    /// Channel it belongs to.
    pub channel_id: &'a str,
    /// Message identifier.
    pub message_id: &'a str,
    /// Verified sender principal, resolved from the roster.
    pub sender_principal: &'a str,
    /// Verified sender device, resolved from the roster.
    pub sender_device: &'a str,
    /// Position in this device's send sequence.
    pub device_sequence: u64,
    /// This message's chain link.
    pub chain_id: &'a str,
    /// The link it claims to follow, or `None` for a device's first message.
    pub previous_chain_id: Option<&'a str>,
    /// Local recipient device selecting recipient-scoped ordering.
    ///
    /// `None` selects the legacy global predecessor carried by older
    /// envelopes. `Some` uses the separate recipient-ordering tables.
    pub recipient_device: Option<&'a str>,
    /// The predecessor carried for `recipient_device`, including `None` for
    /// that recipient's first message.
    pub recipient_previous_chain_id: Option<&'a str>,
    /// Enumerated kind, or `unsupported`.
    pub kind: &'a str,
    /// Thread it belongs to.
    pub thread_id: Option<&'a str>,
    /// Message it answers, if any.
    pub in_reply_to: Option<&'a str>,
    /// Roster epoch it was encrypted under.
    pub roster_epoch: u64,
    /// Validated endpoint identifier, or `None`.
    pub endpoint: Option<&'a str>,
    /// Size of the ciphertext.
    pub ciphertext_bytes: u64,
    /// Size of the decrypted content.
    pub plaintext_bytes: u64,
    /// Digest of the ciphertext it arrived as.
    pub ciphertext_sha256: &'a str,
    /// Sender-declared creation time.
    pub created_at: &'a str,
    /// Sender-declared expiry.
    pub expires_at: Option<&'a str>,
    /// Whether core authenticated the message but classified it as expired.
    pub expired: bool,
    /// How many attachments the signed envelope declared.
    pub attachment_count: u32,
    /// Total declared ciphertext size of those attachments.
    pub attachment_bytes: u64,
    /// Whether the sender requested the `prompt:request` capability.
    pub prompt_request: bool,
    /// The quarantined plaintext.
    pub body: &'a [u8],
    /// The ciphertext, kept while a message is held so it can be replayed.
    pub ciphertext: &'a [u8],
    /// A verified context package, if the message carried one.
    pub context: Option<InboundContext<'a>>,
}

/// A context package received with an inbound message.
#[derive(Debug, Clone)]
pub struct InboundContext<'a> {
    /// Sender-selected package identifier.
    pub package_id: &'a str,
    /// Verified SHA-256 digest of the manifest.
    pub digest: &'a str,
    /// Canonical context manifest bytes.
    pub manifest: &'a [u8],
}

/// A locally drafted package ready to be checked and sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDraft {
    /// Sender-selected package identifier.
    pub package_id: String,
    /// Repository used to evaluate ignore rules, when excerpts are present.
    pub repository_root: Option<String>,
    /// SHA-256 digest of the canonical manifest.
    pub digest: String,
    /// Canonical context manifest bytes.
    pub manifest: Vec<u8>,
}

/// What this installation remembers about a message it sent.
///
/// Enough to judge a receipt and nothing more: who it was addressed to, which
/// thread it belongs to, and what kind it was.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMessageFacts {
    /// The message identifier.
    pub message_id: String,
    /// The thread it belongs to.
    pub thread_id: String,
    /// Its kind.
    pub kind: String,
    /// The devices it was encrypted to.
    pub recipient_device_ids: Vec<String>,
}

/// One entry as the inbox reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InboxEntry {
    /// Message identifier.
    pub message_id: String,
    /// Verified sender principal.
    pub sender_principal: String,
    /// Verified sender device.
    pub sender_device: String,
    /// Enumerated kind.
    pub kind: String,
    /// Thread it belongs to.
    pub thread_id: Option<String>,
    /// Message it answers.
    pub in_reply_to: Option<String>,
    /// Local arrival order.
    pub arrival_sequence: u64,
    /// Sender-declared expiry, when the message has one.
    pub expires_at: Option<String>,
    /// Current disposition.
    pub disposition: String,
}

/// One inbox row as the Herdr plugin renders it (PRD section 23.2).
///
/// Every field is either locally observed or a validated identifier. There
/// is no body field, and adding one would be the change that makes the
/// plugin's inbox an agent-readable surface for pending content.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginInboxEntry {
    /// Message identifier.
    pub message_id: String,
    /// Verified sender principal, resolved from the roster.
    pub sender_principal: String,
    /// Enumerated kind, or `unsupported`.
    pub kind: String,
    /// Thread it belongs to, as recorded.
    pub thread_id: Option<String>,
    /// Validated endpoint identifier, or `None`.
    pub endpoint: Option<String>,
    /// Size of the ciphertext.
    pub ciphertext_bytes: u64,
    /// Size of the decrypted content.
    pub plaintext_bytes: u64,
    /// Sender-declared creation time.
    pub created_at: String,
    /// When this installation accepted it.
    pub received_at: String,
    /// Sender-declared expiry.
    pub expires_at: Option<String>,
    /// How many attachments the envelope declared.
    pub attachment_count: u32,
    /// Total declared ciphertext size of those attachments.
    pub attachment_bytes: u64,
    /// Whether the sender requested the prompt capability.
    pub prompt_request: bool,
    /// Current disposition.
    pub disposition: String,
}

/// The trusted prompt-gate view of a pending inbox row.
///
/// This is deliberately not used by agent-safe queries. It contains exactly
/// the verified metadata and quarantined body needed to reconstruct the
/// gate's authorization binding after a daemon restart.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingInbound {
    /// Channel identity.
    pub channel_id: String,
    /// Message identity.
    pub message_id: String,
    /// Verified sender principal and device.
    pub sender_principal: String,
    pub sender_device: String,
    /// Signed-envelope metadata used by the prompt gate.
    pub kind: String,
    pub roster_epoch: u64,
    pub endpoint: Option<String>,
    pub created_at: String,
    pub expires_at: Option<String>,
    /// Ciphertext digest to which approval binds.
    pub ciphertext_sha256: String,
    /// Canonical pending content.
    pub body: String,
}

/// One receipt, as recorded locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedReceipt {
    /// The message reported on.
    pub message_id: String,
    /// Verified reporting principal.
    pub reporter_principal: String,
    /// Verified reporting device.
    pub reporter_device: String,
    /// The state reported.
    pub state: String,
    /// Why, when the state is a rejection.
    pub rejection_code: Option<String>,
    /// The reporter's timestamp.
    pub reported_at: String,
}

/// One approval decision, as the audit log preserves it.
///
/// Carries content rather than only digests. The question asked of an audit
/// trail later is not "was this approved" but "what was approved, and what
/// did the human change before it reached an agent" — and a digest cannot
/// answer the second half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// The channel it belongs to.
    pub channel_id: String,
    /// The message decided on.
    pub message_id: String,
    /// What was decided, for example `deliver_to_agent`.
    pub action: String,
    /// The content as it arrived.
    pub original_content: String,
    /// What was delivered instead, when the human edited it.
    pub edited_content: Option<String>,
    /// Digest of the original.
    pub content_hash: String,
    /// Digest of the edit.
    pub edited_hash: Option<String>,
    /// The local agent the human chose, if the decision reached one.
    pub agent: Option<String>,
    /// The local human who decided.
    pub decided_by: String,
    /// When.
    pub occurred_at: String,
}

/// One invite this installation issued.
///
/// The secret is absent by construction: it is handed to the invited person
/// and never stored, because an invite secret at rest is a second copy of
/// something that only ever needed to exist once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRecord {
    /// The invite's published identifier.
    pub invite_id: String,
    /// Who the administrator meant it for, in their own words.
    pub intended_for: String,
    /// When it lapses.
    pub expires_at: String,
    /// `open`, `consumed`, or `revoked`.
    pub state: String,
}

/// One recorded audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Channel the action applied to, when any.
    pub channel_id: Option<String>,
    /// Message the action applied to, when any.
    pub message_id: Option<String>,
    /// Stable action name.
    pub action: String,
    /// Optional detail for a human.
    pub detail: Option<String>,
    /// RFC 3339 time the action occurred.
    pub occurred_at: String,
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
                        roster_epoch, control_sequence, sync_cursor, receive_cursor,
                        halted_reason
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
                        receive_cursor: row.get(7)?,
                        halted_reason: row.get(8)?,
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

    /// Advances the durable cursor for locally received message bodies.
    ///
    /// This is deliberately independent of the transport cursor: locked
    /// synchronization may validate and cache transport history without
    /// claiming that encrypted message bodies were processed.
    pub fn set_receive_cursor(&self, channel_id: &str, cursor: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET receive_cursor = ?2 WHERE channel_id = ?1",
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

        ensure_channel(&transaction, channel_id)?;
        let (device_sequence, previous_chain_id) =
            increment_device_sequence(&transaction, channel_id, device_id)?;

        let chain_id = chain_id_for(channel_id, device_id, device_sequence, message_id)?;

        set_last_allocated_chain(&transaction, channel_id, device_id, &chain_id)?;

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
            recipient_previous_chain_ids: BTreeMap::new(),
        })
    }

    /// Allocates, builds, and queues one outgoing message atomically.
    ///
    /// The builder runs while an immediate transaction holds the writer lock.
    /// If recipient validation, sealing, or protected-material construction
    /// fails, the sequence allocation, per-recipient predecessor selection,
    /// outbox row, and recipient facts all roll back together.
    #[allow(clippy::too_many_arguments)]
    pub fn compose_outgoing<E, F>(
        &mut self,
        channel_id: &str,
        device_id: &str,
        message_id: &str,
        roster_epoch: u64,
        payload_hash: &str,
        thread_id: &str,
        kind: &str,
        recipient_device_ids: &[String],
        now: &str,
        build: F,
    ) -> std::result::Result<OutgoingReservation, E>
    where
        E: From<StorageError>,
        F: FnOnce(&OutgoingReservation) -> std::result::Result<OutgoingBuild, E>,
    {
        let recipients = hrc_protocol::RecipientDevices::new(recipient_device_ids.to_vec())
            .map_err(StorageError::from)
            .map_err(E::from)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StorageError::from)
            .map_err(E::from)?;

        ensure_channel(&transaction, channel_id).map_err(E::from)?;
        let (device_sequence, _) =
            increment_device_sequence(&transaction, channel_id, device_id).map_err(E::from)?;

        // New recipient-ordered messages never link through a legacy
        // reservation that may later be burned as a gap.
        let previous_chain_id =
            latest_publishable_chain(&transaction, channel_id, device_id, device_sequence)
                .map_err(E::from)?;
        let chain_id =
            chain_id_for(channel_id, device_id, device_sequence, message_id).map_err(E::from)?;
        let recipient_previous_chain_ids = recipient_predecessors_for(
            &transaction,
            channel_id,
            device_id,
            device_sequence,
            &recipients.device_ids,
        )
        .map_err(E::from)?;

        let reservation = OutgoingReservation {
            message_id: message_id.to_owned(),
            device_sequence,
            chain_id,
            previous_chain_id,
            roster_epoch,
            recipient_previous_chain_ids,
        };
        let built = build(&reservation)?;

        transaction
            .execute(
                "INSERT INTO outbox (
                     message_id, channel_id, device_id, device_sequence, chain_id,
                     previous_chain_id, roster_epoch, payload_hash, ciphertext,
                     reseal_material, thread_id, kind, state, created_at, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12,
                     'queued', ?13, ?13
                 )",
                params![
                    &reservation.message_id,
                    channel_id,
                    device_id,
                    reservation.device_sequence as i64,
                    &reservation.chain_id,
                    reservation.previous_chain_id.as_deref(),
                    roster_epoch as i64,
                    payload_hash,
                    &built.ciphertext,
                    &built.reseal_material,
                    thread_id,
                    kind,
                    now,
                ],
            )
            .map_err(StorageError::from)
            .map_err(E::from)?;

        replace_recipient_facts(&transaction, message_id, &recipients.device_ids)
            .map_err(E::from)?;
        set_last_allocated_chain(&transaction, channel_id, device_id, &reservation.chain_id)
            .map_err(E::from)?;

        transaction
            .commit()
            .map_err(StorageError::from)
            .map_err(E::from)?;
        Ok(reservation)
    }

    /// Records an arriving message, deciding whether it is new, a repeat, or
    /// out of order.
    ///
    /// All three decisions and their consequences happen in one immediate
    /// transaction, for the same reason allocation does: two fetches running
    /// at once must not both conclude they hold the next message from a
    /// device, and a release must not interleave with the accept that
    /// triggered it.
    ///
    /// The rules, in the order they are checked:
    ///
    /// 1. A message ID already accepted with the same ciphertext digest is a
    ///    duplicate. At-least-once delivery makes this ordinary, so it is
    ///    reported rather than treated as an error (HRC-MSG-006).
    /// 2. The same message ID with a *different* digest is not a repeat at
    ///    all; it is a substitution, and it halts the channel.
    /// 3. A sender device reusing a sequence number, or naming a predecessor
    ///    other than the one recorded for it, has forked its own chain. That
    ///    is the per-device equivalent of a rewritten control log, and it
    ///    halts the channel too (HRC-MSG-007).
    /// 4. A message whose predecessor has not arrived is held, not dropped
    ///    and not accepted early. Lazy and partial fetching are supported, so
    ///    the predecessor may simply still be in flight — but per-device
    ///    order is a guarantee, so it cannot be accepted ahead of it either.
    /// 5. Otherwise it is accepted, and anything held behind it is released
    ///    in sequence order.
    ///
    /// Receipts participate in the same chain checks, but only their links
    /// are stored: they never acquire an inbox row or an arrival number.
    pub fn record_inbound(
        &mut self,
        message: &InboundMessage<'_>,
        now: &str,
    ) -> Result<InboundOutcome> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;

        // 1 and 2: is this one we already have?
        let existing: Option<Option<String>> = transaction
            .query_row(
                "SELECT ciphertext_sha256 FROM inbox WHERE message_id = ?1 AND channel_id = ?2
                 UNION ALL
                 SELECT ciphertext_sha256 FROM inbound_receipt_link
                 WHERE message_id = ?1 AND channel_id = ?2
                 UNION ALL
                 SELECT ciphertext_sha256 FROM inbound_recipient_link
                 WHERE message_id = ?1 AND channel_id = ?2",
                params![message.message_id, message.channel_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?;

        if let Some(recorded) = existing {
            return if recorded.as_deref() == Some(message.ciphertext_sha256) {
                transaction.commit()?;
                Ok(InboundOutcome::Duplicate)
            } else {
                Err(StorageError::InboundSubstituted {
                    message_id: message.message_id.to_owned(),
                })
            };
        }

        // 3: has this device already used this sequence, under a different
        // identity? A repeat of the same chain link would have been caught
        // above, so reaching here with the sequence taken means a fork.
        let taken: Option<String> = transaction
            .query_row(
                "SELECT message_id FROM inbox
                 WHERE channel_id = ?1 AND sender_device = ?2 AND device_sequence = ?3
                 UNION ALL
                 SELECT message_id FROM inbound_receipt_link
                 WHERE channel_id = ?1 AND sender_device = ?2 AND device_sequence = ?3
                 UNION ALL
                 SELECT message_id FROM inbound_recipient_link
                 WHERE channel_id = ?1 AND sender_device = ?2 AND device_sequence = ?3",
                params![
                    message.channel_id,
                    message.sender_device,
                    message.device_sequence as i64
                ],
                |row| row.get::<_, String>(0),
            )
            .optional()?;

        if let Some(other) = taken {
            return Err(StorageError::InboundForked {
                sender_device: message.sender_device.to_owned(),
                device_sequence: message.device_sequence,
                existing: other,
                arriving: message.message_id.to_owned(),
            });
        }

        let chain_taken: Option<String> = transaction
            .query_row(
                "SELECT message_id FROM inbox
                 WHERE channel_id = ?1 AND chain_id = ?2
                 UNION ALL
                 SELECT message_id FROM inbound_receipt_link
                 WHERE channel_id = ?1 AND chain_id = ?2
                 UNION ALL
                 SELECT message_id FROM inbound_recipient_link
                 WHERE channel_id = ?1 AND chain_id = ?2",
                params![message.channel_id, message.chain_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        if let Some(other) = chain_taken {
            return Err(StorageError::InboundForked {
                sender_device: message.sender_device.to_owned(),
                device_sequence: message.device_sequence,
                existing: other,
                arriving: message.message_id.to_owned(),
            });
        }

        if let Some(recipient_device) = message.recipient_device {
            let outcome = record_recipient_inbound(&transaction, message, recipient_device, now)?;
            transaction.commit()?;
            return Ok(outcome);
        }

        let head: Option<(u64, String)> = transaction
            .query_row(
                "SELECT last_sequence, last_chain_id FROM inbound_chain
                 WHERE channel_id = ?1 AND sender_device = ?2",
                params![message.channel_id, message.sender_device],
                |row| Ok((row.get::<_, i64>(0)? as u64, row.get::<_, String>(1)?)),
            )
            .optional()?;

        if message.kind == "receipt" {
            transaction.execute(
                "INSERT INTO inbound_receipt_link (
                     message_id, channel_id, sender_device, device_sequence,
                     chain_id, previous_chain_id, ciphertext_sha256
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                params![
                    message.message_id,
                    message.channel_id,
                    message.sender_device,
                    message.device_sequence as i64,
                    message.chain_id,
                    message.previous_chain_id,
                    message.ciphertext_sha256,
                ],
            )?;
        }

        match (&head, message.previous_chain_id) {
            // The device's first message, and we have nothing recorded.
            (None, None) => {}

            // A first message that claims a predecessor: either we missed
            // everything before it, or it is lying. Holding covers the first
            // case and costs nothing in the second, since a liar cannot
            // produce the predecessor it invented.
            (None, Some(_)) => {
                let outcome = hold(&transaction, message, now)?;
                store_inbound_context(&transaction, message)?;
                transaction.commit()?;
                return Ok(outcome);
            }

            // The next link in the chain we have.
            (Some((_, last)), Some(previous)) if last == previous => {}

            // A predecessor we have not seen. Same reasoning as above.
            (Some(_), Some(_)) => {
                let outcome = hold(&transaction, message, now)?;
                store_inbound_context(&transaction, message)?;
                transaction.commit()?;
                return Ok(outcome);
            }

            // Claiming to be a device's first message when it is not.
            (Some(_), None) => {
                return Err(StorageError::InboundForked {
                    sender_device: message.sender_device.to_owned(),
                    device_sequence: message.device_sequence,
                    existing: head.map(|(_, chain)| chain).unwrap_or_default(),
                    arriving: message.message_id.to_owned(),
                });
            }
        }

        let arrival_sequence = insert_accepted(&transaction, message, now)?;
        store_inbound_context(&transaction, message)?;

        // 5: anything that was waiting on this link can go in now, and so
        // can anything waiting on *that*, so the release walks the chain.
        let mut released = Vec::new();
        let mut link = message.chain_id.to_owned();
        let mut sequence = message.device_sequence;

        loop {
            if let Some((message_id, device_sequence, chain_id)) = take_held_receipt(
                &transaction,
                message.channel_id,
                message.sender_device,
                &link,
            )? {
                transaction.execute(
                    "DELETE FROM inbound_hold WHERE message_id = ?1",
                    params![message_id],
                )?;
                sequence = device_sequence;
                link = chain_id;
                continue;
            }

            let Some(next) = take_held(
                &transaction,
                message.channel_id,
                message.sender_device,
                &link,
            )?
            else {
                break;
            };
            let body = next.body.clone();
            let waiting = InboundMessage {
                channel_id: message.channel_id,
                message_id: &next.message_id,
                sender_principal: &next.sender_principal,
                sender_device: message.sender_device,
                device_sequence: next.device_sequence,
                chain_id: &next.chain_id,
                previous_chain_id: next.previous_chain_id.as_deref(),
                recipient_device: None,
                recipient_previous_chain_id: None,
                kind: &next.kind,
                thread_id: next.thread_id.as_deref(),
                in_reply_to: next.in_reply_to.as_deref(),
                roster_epoch: next.roster_epoch,
                endpoint: next.endpoint.as_deref(),
                ciphertext_bytes: next.ciphertext_bytes,
                plaintext_bytes: next.plaintext_bytes,
                ciphertext_sha256: &next.ciphertext_sha256,
                created_at: &next.created_at,
                expires_at: next.expires_at.as_deref(),
                expired: next.disposition == "expired",
                attachment_count: next.attachment_count,
                attachment_bytes: next.attachment_bytes,
                prompt_request: next.prompt_request,
                body: &body,
                ciphertext: &[],
                context: None,
            };

            insert_accepted(&transaction, &waiting, now)?;
            if next.disposition != "expired" {
                released.push(next.message_id.clone());
            }
            sequence = next.device_sequence;
            link = next.chain_id;
        }

        transaction.execute(
            "INSERT INTO inbound_chain (channel_id, sender_device, last_sequence, last_chain_id)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (channel_id, sender_device)
             DO UPDATE SET last_sequence = excluded.last_sequence,
                           last_chain_id = excluded.last_chain_id",
            params![
                message.channel_id,
                message.sender_device,
                sequence as i64,
                link,
            ],
        )?;

        transaction.commit()?;

        Ok(match (arrival_sequence, message.expired) {
            (Some(_), true) => InboundOutcome::ExpiredAccepted { released },
            (Some(arrival_sequence), false) => InboundOutcome::Accepted {
                arrival_sequence,
                released,
            },
            (None, true) => InboundOutcome::ExpiredAccepted { released },
            (None, false) => InboundOutcome::ReceiptAccepted { released },
        })
    }

    /// Records an invite this installation issued.
    pub fn record_invite(
        &self,
        channel_id: &str,
        invite_id: &str,
        intended_for: &str,
        expires_at: &str,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO invite (invite_id, channel_id, intended_for, expires_at, created_at, state)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open')",
            params![invite_id, channel_id, intended_for, expires_at, now],
        )?;

        Ok(())
    }

    /// Invites issued for a channel, newest first.
    pub fn invites(&self, channel_id: &str) -> Result<Vec<InviteRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT invite_id, intended_for, expires_at, state FROM invite
             WHERE channel_id = ?1
             ORDER BY created_at DESC, invite_id",
        )?;

        let rows = statement.query_map(params![channel_id], |row| {
            Ok(InviteRecord {
                invite_id: row.get(0)?,
                intended_for: row.get(1)?,
                expires_at: row.get(2)?,
                state: row.get(3)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Updates an invite's locally recorded state.
    pub fn set_invite_state(&self, channel_id: &str, invite_id: &str, state: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE invite SET state = ?3 WHERE channel_id = ?1 AND invite_id = ?2",
            params![channel_id, invite_id, state],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownInvite {
                invite_id: invite_id.to_owned(),
            });
        }

        Ok(())
    }

    /// Appends one approval decision.
    ///
    /// There is no update or delete path for this table, here or anywhere
    /// else in the storage API. A decision that could be revised afterwards
    /// would not be evidence of anything.
    pub fn append_decision(&self, decision: &DecisionRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO audit (
                 channel_id, message_id, action, content_hash, edited_hash,
                 original_content, edited_content, agent, decided_by, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                decision.channel_id,
                decision.message_id,
                decision.action,
                decision.content_hash,
                decision.edited_hash,
                decision.original_content.as_bytes(),
                decision.edited_content.as_deref().map(str::as_bytes),
                decision.agent,
                decision.decided_by,
                decision.occurred_at,
            ],
        )?;

        Ok(())
    }

    /// Commits a prompt-gate decision and the inbox/context lifecycle change
    /// as one transaction. Context has no separate disposition, so the
    /// foreign key makes this state transition govern it too.
    pub fn commit_decision(&mut self, decision: &DecisionRecord) -> Result<()> {
        let disposition = match decision.action.as_str() {
            "deliver_to_agent" => "approved",
            "deliver_edited" => "edited",
            "decline" => "declined",
            // An inbox-only decision intentionally leaves the message (and
            // its context) behind the trusted quarantine boundary.
            "keep_in_inbox" => "quarantined",
            _ => {
                return Err(StorageError::InvalidDecisionAction {
                    action: decision.action.clone(),
                });
            }
        };
        let transaction = self.connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE inbox SET disposition = ?3
             WHERE channel_id = ?1 AND message_id = ?2 AND disposition = 'quarantined'",
            params![decision.channel_id, decision.message_id, disposition],
        )?;
        if updated != 1 {
            return Err(StorageError::UnknownMessage {
                message_id: decision.message_id.clone(),
            });
        }
        transaction.execute(
            "INSERT INTO audit (
                 channel_id, message_id, action, content_hash, edited_hash,
                 original_content, edited_content, agent, decided_by, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                decision.channel_id,
                decision.message_id,
                decision.action,
                decision.content_hash,
                decision.edited_hash,
                decision.original_content.as_bytes(),
                decision.edited_content.as_deref().map(str::as_bytes),
                decision.agent,
                decision.decided_by,
                decision.occurred_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every decision recorded about one message, oldest first.
    pub fn decisions_for(&self, message_id: &str) -> Result<Vec<DecisionRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, message_id, action, content_hash, edited_hash,
                    original_content, edited_content, agent, decided_by, occurred_at
             FROM audit
             WHERE message_id = ?1 AND decided_by IS NOT NULL
             ORDER BY id",
        )?;

        let rows = statement.query_map(params![message_id], |row| {
            Ok(DecisionRecord {
                channel_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                message_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                action: row.get(2)?,
                content_hash: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                edited_hash: row.get(4)?,
                original_content: row
                    .get::<_, Option<Vec<u8>>>(5)?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default(),
                edited_content: row
                    .get::<_, Option<Vec<u8>>>(6)?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                agent: row.get(7)?,
                decided_by: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                occurred_at: row.get(9)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Records one device's report about one message.
    ///
    /// Returns whether this was new. A repeat of a state a device already
    /// reported is ordinary: receipts ride the same at-least-once transport
    /// as everything else, so the same report can arrive twice.
    pub fn record_receipt(
        &self,
        channel_id: &str,
        receipt: &RecordedReceipt,
        now: &str,
    ) -> Result<bool> {
        let inserted = self.connection.execute(
            "INSERT INTO delivery_receipt (
                 message_id, channel_id, reporter_principal, reporter_device,
                 state, rejection_code, reported_at, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (message_id, reporter_device, state) DO NOTHING",
            params![
                receipt.message_id,
                channel_id,
                receipt.reporter_principal,
                receipt.reporter_device,
                receipt.state,
                receipt.rejection_code,
                receipt.reported_at,
                now,
            ],
        )?;

        Ok(inserted == 1)
    }

    /// Every receipt recorded for one message, in local recording order.
    pub fn receipts_for(&self, message_id: &str) -> Result<Vec<RecordedReceipt>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, reporter_principal, reporter_device, state,
                    rejection_code, reported_at
             FROM delivery_receipt
             WHERE message_id = ?1
             ORDER BY recorded_at, reporter_device, state",
        )?;

        let rows = statement.query_map(params![message_id], |row| {
            Ok(RecordedReceipt {
                message_id: row.get(0)?,
                reporter_principal: row.get(1)?,
                reporter_device: row.get(2)?,
                state: row.get(3)?,
                rejection_code: row.get(4)?,
                reported_at: row.get(5)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The devices that have reported a given state for a message.
    pub fn devices_reporting(&self, message_id: &str, state: &str) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT reporter_device FROM delivery_receipt
             WHERE message_id = ?1 AND state = ?2
             ORDER BY reporter_device",
        )?;
        let rows =
            statement.query_map(params![message_id, state], |row| row.get::<_, String>(0))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Messages held because their predecessor has not arrived.
    pub fn held_inbound(&self, channel_id: &str) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, sender_device, device_sequence
             FROM inbound_hold WHERE channel_id = ?1
             UNION ALL
             SELECT message_id, sender_device, device_sequence
             FROM inbound_recipient_link
             WHERE channel_id = ?1 AND state = 'held'
             ORDER BY sender_device, device_sequence",
        )?;
        let rows = statement.query_map(params![channel_id], |row| row.get::<_, String>(0))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Accepted inbox entries in local arrival order.
    pub fn inbox_entries(&self, channel_id: &str) -> Result<Vec<InboxEntry>> {
        self.query_inbox(channel_id, None)
    }

    /// Marks every quarantined message whose expiry has passed.
    ///
    /// PRD section 26 requires an expired message to be displayed as expired
    /// and to allow no action, and section 16.3 says expiration is logical:
    /// the row stays and the published history is untouched. What does go is
    /// the quarantined plaintext. A body the local human can no longer act
    /// on has no remaining purpose, and an unapproved remote body sitting in
    /// local storage indefinitely is precisely what the quarantine model
    /// exists to avoid. The object is still in Git if it is ever needed
    /// again, because that history is immutable.
    ///
    /// Only `quarantined` rows are swept. A message already approved,
    /// edited, or declined has had its decision, and a decision does not
    /// lapse.
    ///
    /// Returns the message IDs swept, so a caller can report and audit them.
    pub fn sweep_expired(&self, now: &str) -> Result<Vec<String>> {
        let swept: Vec<String> = {
            let mut statement = self.connection.prepare(
                "SELECT message_id FROM inbox
                 WHERE disposition = 'quarantined'
                   AND expires_at IS NOT NULL
                   AND expires_at <= ?1
                 ORDER BY COALESCE(arrival_sequence, 9223372036854775807), message_id",
            )?;
            let rows = statement.query_map(params![now], |row| row.get::<_, String>(0))?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };

        if swept.is_empty() {
            return Ok(swept);
        }

        self.connection.execute(
            "UPDATE inbox SET disposition = 'expired', body = NULL
             WHERE disposition = 'quarantined'
               AND expires_at IS NOT NULL
               AND expires_at <= ?1",
            params![now],
        )?;

        Ok(swept)
    }

    /// Returns the metadata the Herdr plugin inbox renders.
    ///
    /// Deliberately separate from [`Database::pending_inbound`], which
    /// carries bodies: the plugin's inbox surface never holds one, so the
    /// query that feeds it does not select the column. The `body` column is
    /// not in the SELECT list at all, which is a stronger guarantee than
    /// discarding it afterwards.
    ///
    /// Ordered by local arrival rather than by the sender's timestamp. A
    /// sender chooses `created_at`, and should not get to choose where their
    /// message sits in someone else's inbox.
    pub fn plugin_inbox(&self, channel_id: &str) -> Result<Vec<PluginInboxEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, sender_principal, kind, thread_id, endpoint,
                    ciphertext_bytes, plaintext_bytes, created_at, received_at, expires_at,
                    attachment_count, attachment_bytes, prompt_request, disposition
             FROM inbox
             WHERE channel_id = ?1 AND arrival_sequence IS NOT NULL
             ORDER BY arrival_sequence",
        )?;
        let rows = statement.query_map(params![channel_id], |row| {
            Ok(PluginInboxEntry {
                message_id: row.get(0)?,
                sender_principal: row.get(1)?,
                kind: row.get(2)?,
                thread_id: row.get(3)?,
                endpoint: row.get(4)?,
                ciphertext_bytes: row.get::<_, i64>(5)? as u64,
                plaintext_bytes: row.get::<_, i64>(6)? as u64,
                created_at: row.get(7)?,
                received_at: row.get(8)?,
                expires_at: row.get(9)?,
                attachment_count: row.get::<_, i64>(10)? as u32,
                attachment_bytes: row.get::<_, i64>(11)? as u64,
                prompt_request: row.get(12)?,
                disposition: row.get(13)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Records that a notification was raised, and says whether it is new.
    ///
    /// Returns `true` the first time a message-and-kind pair is seen and
    /// `false` every time after, including after a restart — which is the
    /// whole point. Deduplication held in a process that lives for one pane
    /// render is no deduplication at all.
    ///
    /// One statement rather than a read and a write, so two panes refreshing
    /// at the same moment cannot both decide they are the first.
    /// `ON CONFLICT DO NOTHING` makes the insert the test: the row is
    /// created and the notification is new, or the row was already there and
    /// it is not.
    pub fn record_notification(
        &self,
        channel_id: &str,
        message_id: &str,
        kind: &str,
        now: &str,
    ) -> Result<bool> {
        let inserted = self.connection.execute(
            "INSERT INTO notification (message_id, kind, channel_id, notified_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (message_id, kind) DO NOTHING",
            params![message_id, kind, channel_id, now],
        )?;

        Ok(inserted == 1)
    }

    /// Marks every notification about a message as no longer outstanding.
    ///
    /// Called when a message is decided or expires. The row stays, carrying
    /// the time it was resolved: a deleted row and a row that was never
    /// written are indistinguishable, and telling them apart is what stops a
    /// decided message being announced again.
    pub fn resolve_notifications(&self, message_id: &str, now: &str) -> Result<usize> {
        Ok(self.connection.execute(
            "UPDATE notification SET resolved_at = ?2
             WHERE message_id = ?1 AND resolved_at IS NULL",
            params![message_id, now],
        )?)
    }

    /// The notifications raised about a channel and not yet resolved.
    ///
    /// Returns `(message_id, kind)` pairs, oldest first.
    pub fn outstanding_notifications(&self, channel_id: &str) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, kind FROM notification
             WHERE channel_id = ?1 AND resolved_at IS NULL
             ORDER BY notified_at, message_id",
        )?;
        let rows =
            statement.query_map(params![channel_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Returns every body still owned by the trusted prompt gate.
    pub fn pending_inbound(&self) -> Result<Vec<PendingInbound>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, message_id, sender_principal, sender_device, kind,
                    roster_epoch, endpoint, created_at, expires_at,
                    ciphertext_sha256, body
             FROM inbox
             WHERE disposition = 'quarantined' AND arrival_sequence IS NOT NULL
             ORDER BY received_at, message_id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(PendingInbound {
                channel_id: row.get(0)?,
                message_id: row.get(1)?,
                sender_principal: row.get(2)?,
                sender_device: row.get(3)?,
                kind: row.get(4)?,
                roster_epoch: row.get::<_, i64>(5)? as u64,
                endpoint: row.get(6)?,
                created_at: row.get(7)?,
                expires_at: row.get(8)?,
                ciphertext_sha256: row.get(9)?,
                body: String::from_utf8(row.get::<_, Option<Vec<u8>>>(10)?.unwrap_or_default())
                    .unwrap_or_default(),
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Stores a locally authored package without publishing it.
    pub fn save_context_draft(
        &self,
        package_id: &str,
        repository_root: Option<&str>,
        digest: &str,
        manifest: &[u8],
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO context_draft (package_id, repository_root, digest, manifest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![package_id, repository_root, digest, manifest, now],
        )?;
        Ok(())
    }

    /// Returns one locally authored package.
    pub fn context_draft(&self, package_id: &str) -> Result<Option<ContextDraft>> {
        self.connection
            .query_row(
                "SELECT package_id, repository_root, digest, manifest
                 FROM context_draft WHERE package_id = ?1",
                params![package_id],
                |row| {
                    Ok(ContextDraft {
                        package_id: row.get(0)?,
                        repository_root: row.get(1)?,
                        digest: row.get(2)?,
                        manifest: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every locally authored package, oldest first.
    ///
    /// Only the identifier and digest, not the manifest. A caller listing
    /// what could be sent has no business reading the contents of each one,
    /// and the manifests are large enough that loading them all to render a
    /// menu would be wasteful as well as wrong.
    pub fn context_drafts(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT package_id, digest FROM context_draft ORDER BY created_at, package_id",
        )?;

        let drafts = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(drafts)
    }

    /// Reads quarantined context only for trusted code and tests.
    pub fn inbound_context(&self, message_id: &str) -> Result<Option<ContextDraft>> {
        self.connection
            .query_row(
                "SELECT context.package_id, NULL, context.digest, context.manifest
                 FROM inbound_context AS context
                 JOIN inbox ON inbox.message_id = context.message_id
                 WHERE context.message_id = ?1 AND inbox.disposition = 'quarantined'",
                params![message_id],
                |row| {
                    Ok(ContextDraft {
                        package_id: row.get(0)?,
                        repository_root: row.get(1)?,
                        digest: row.get(2)?,
                        manifest: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Rejects one signed message whose attached context is malformed.
    ///
    /// This is sender-authored content, not a transport/history failure, so
    /// unrelated traffic must continue to synchronize.
    pub fn reject_malformed_context(
        &mut self,
        channel_id: &str,
        message_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE inbox SET disposition = 'unsupported'
             WHERE channel_id = ?1 AND message_id = ?2 AND disposition = 'quarantined'",
            params![channel_id, message_id],
        )?;
        if updated == 1 {
            transaction.execute(
                "INSERT INTO audit (
                     channel_id, message_id, action, content_hash, detail, occurred_at
                 ) VALUES (?1, ?2, 'malformed_context', NULL, ?3, ?4)",
                params![channel_id, message_id, reason, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }

    /// One thread's entries, in local arrival order.
    ///
    /// Ordering is by arrival rather than by the sender's `created_at`,
    /// which is a value the sender chooses: a backdated timestamp would
    /// otherwise let a remote peer place its message anywhere in someone
    /// else's reading of the conversation.
    pub fn thread_entries(&self, channel_id: &str, thread_id: &str) -> Result<Vec<InboxEntry>> {
        self.query_inbox(channel_id, Some(thread_id))
    }

    /// Shared inbox query, optionally narrowed to one thread.
    ///
    /// A held message has a row — its body is stored so releasing it does not
    /// mean decrypting again — but no arrival number, and it is not in the
    /// inbox until it has one. Listing it earlier would present a message
    /// whose place in its sender's history is not yet established.
    fn query_inbox(&self, channel_id: &str, thread_id: Option<&str>) -> Result<Vec<InboxEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, sender_principal, sender_device, kind, thread_id,
                    in_reply_to, arrival_sequence, expires_at, disposition
             FROM inbox
             WHERE channel_id = ?1 AND (?2 IS NULL OR thread_id = ?2)
               AND arrival_sequence IS NOT NULL
             ORDER BY arrival_sequence",
        )?;

        let rows = statement.query_map(params![channel_id, thread_id], |row| {
            Ok(InboxEntry {
                message_id: row.get(0)?,
                sender_principal: row.get(1)?,
                sender_device: row.get(2)?,
                kind: row.get(3)?,
                thread_id: row.get(4)?,
                in_reply_to: row.get(5)?,
                arrival_sequence: row.get::<_, Option<i64>>(6)?.unwrap_or(0) as u64,
                expires_at: row.get(7)?,
                disposition: row.get(8)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Attaches ciphertext to a reserved slot and queues it for publication.
    pub fn queue_outgoing(&self, message_id: &str, ciphertext: &[u8], now: &str) -> Result<()> {
        self.queue_outgoing_resealable(message_id, ciphertext, None, now)
    }

    /// Attaches ciphertext and protected re-encryption material to a reserved slot.
    pub fn queue_outgoing_resealable(
        &self,
        message_id: &str,
        ciphertext: &[u8],
        reseal_material: Option<&[u8]>,
        now: &str,
    ) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE outbox
             SET ciphertext = ?2, reseal_material = COALESCE(?3, reseal_material),
                 state = 'queued', updated_at = ?4
             WHERE message_id = ?1 AND state IN ('reserved', 'queued')",
            params![message_id, ciphertext, reseal_material, now],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Records what a sender must remember to judge a receipt later.
    ///
    /// `accept_receipt` refuses a report from a device that was never
    /// addressed, and it can only do that against what this installation
    /// actually sent. These facts are stored rather than read back out of the
    /// published object on purpose: the published envelope is the sender's
    /// own claim, and checking a receipt against whatever the transport holds
    /// now would let a rewritten history validate its own receipts.
    ///
    /// Idempotent, because a resealed message is queued again and the facts
    /// it carries do not change with the epoch.
    pub fn record_sent_facts(
        &mut self,
        message_id: &str,
        thread_id: &str,
        kind: &str,
        recipient_device_ids: &[String],
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;

        let updated = transaction.execute(
            "UPDATE outbox SET thread_id = ?2, kind = ?3 WHERE message_id = ?1",
            params![message_id, thread_id, kind],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            });
        }

        for device_id in recipient_device_ids {
            transaction.execute(
                "INSERT INTO outbox_recipient (message_id, device_id) VALUES (?1, ?2)
                 ON CONFLICT (message_id, device_id) DO NOTHING",
                params![message_id, device_id],
            )?;
        }

        transaction.commit()?;
        Ok(())
    }

    /// What this installation sent on one channel, for judging receipts.
    ///
    /// Only messages whose facts were recorded are returned. A row from
    /// before migration 009 has no recipients, and treating an empty
    /// recipient set as "everyone is allowed" would accept exactly the
    /// reports this check exists to refuse.
    pub fn sent_messages(&self, channel_id: &str) -> Result<Vec<SentMessageFacts>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, thread_id, kind FROM outbox
             WHERE channel_id = ?1 AND thread_id IS NOT NULL AND kind IS NOT NULL",
        )?;

        let rows = statement
            .query_map(params![channel_id], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        let mut sent = Vec::with_capacity(rows.len());
        for (message_id, thread_id, kind) in rows {
            let mut devices = self.connection.prepare(
                "SELECT device_id FROM outbox_recipient
                     WHERE message_id = ?1 ORDER BY device_id",
            )?;
            let recipient_device_ids = devices
                .query_map(params![&message_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;

            if recipient_device_ids.is_empty() {
                continue;
            }

            sent.push(SentMessageFacts {
                message_id,
                thread_id,
                kind,
                recipient_device_ids,
            });
        }

        Ok(sent)
    }

    /// Atomically replaces stale ciphertext with bytes sealed for a newer roster.
    ///
    /// Only the epoch and ciphertext change. Message identity, chain position,
    /// thread data (inside the protected envelope), and payload hash remain
    /// untouched.
    pub fn replace_outgoing_ciphertext(
        &self,
        message_id: &str,
        expected_epoch: u64,
        roster_epoch: u64,
        ciphertext: &[u8],
        now: &str,
    ) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE outbox SET roster_epoch = ?3, ciphertext = ?4, updated_at = ?5
             WHERE message_id = ?1 AND roster_epoch = ?2
               AND state IN ('queued', 'publishing')",
            params![
                message_id,
                expected_epoch as i64,
                roster_epoch as i64,
                ciphertext,
                now
            ],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            });
        }
        Ok(())
    }

    /// Rebuilds stale ciphertext and replaces its exact recipient facts atomically.
    ///
    /// The predecessor map is computed from earlier queued or published
    /// messages, never from the stale envelope. The builder runs inside the
    /// transaction so a failure preserves the old epoch, bytes, and recipient
    /// rows as one consistent snapshot.
    pub fn reseal_outgoing<E, F>(
        &mut self,
        message_id: &str,
        expected_epoch: u64,
        roster_epoch: u64,
        recipient_device_ids: &[String],
        now: &str,
        build: F,
    ) -> std::result::Result<(), E>
    where
        E: From<StorageError>,
        F: FnOnce(&PendingOutgoing, &RecipientPredecessors) -> std::result::Result<Vec<u8>, E>,
    {
        let recipients = hrc_protocol::RecipientDevices::new(recipient_device_ids.to_vec())
            .map_err(StorageError::from)
            .map_err(E::from)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(StorageError::from)
            .map_err(E::from)?;

        let outgoing = pending_outgoing_in_transaction(&transaction, message_id)
            .map_err(E::from)?
            .filter(|outgoing| outgoing.roster_epoch == expected_epoch)
            .ok_or_else(|| {
                E::from(StorageError::UnknownMessage {
                    message_id: message_id.to_owned(),
                })
            })?;
        let channel_id = outgoing_channel(&transaction, message_id).map_err(E::from)?;
        let predecessors = recipient_predecessors_for(
            &transaction,
            &channel_id,
            &outgoing.device_id,
            outgoing.device_sequence,
            &recipients.device_ids,
        )
        .map_err(E::from)?;
        let ciphertext = build(&outgoing, &predecessors)?;

        let updated = transaction
            .execute(
                "UPDATE outbox
                 SET roster_epoch = ?3, ciphertext = ?4, recipient_order_stale = 0,
                     updated_at = ?5
                 WHERE message_id = ?1 AND roster_epoch = ?2
                   AND state IN ('queued', 'publishing')",
                params![
                    message_id,
                    expected_epoch as i64,
                    roster_epoch as i64,
                    ciphertext,
                    now
                ],
            )
            .map_err(StorageError::from)
            .map_err(E::from)?;
        if updated == 0 {
            return Err(E::from(StorageError::UnknownMessage {
                message_id: message_id.to_owned(),
            }));
        }

        replace_recipient_facts(&transaction, message_id, &recipients.device_ids)
            .map_err(E::from)?;
        transaction
            .execute(
                "UPDATE outbox SET recipient_order_stale = 1
                 WHERE channel_id = ?1 AND device_id = ?2 AND device_sequence > ?3
                   AND state IN ('queued', 'publishing')",
                params![
                    channel_id,
                    &outgoing.device_id,
                    outgoing.device_sequence as i64
                ],
            )
            .map_err(StorageError::from)
            .map_err(E::from)?;
        transaction
            .commit()
            .map_err(StorageError::from)
            .map_err(E::from)?;
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

    /// Queued outbox records with the bytes needed for publication, in send order.
    pub fn pending_outgoing_records(&self, channel_id: &str) -> Result<Vec<PendingOutgoing>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, device_id, device_sequence, chain_id,
                    previous_chain_id, roster_epoch, payload_hash, created_at,
                    ciphertext, reseal_material, recipient_order_stale
             FROM outbox
             WHERE channel_id = ?1 AND state IN ('queued', 'publishing')
             ORDER BY device_sequence",
        )?;
        let rows = statement.query_map(params![channel_id], |row| {
            Ok(PendingOutgoing {
                message_id: row.get(0)?,
                device_id: row.get(1)?,
                device_sequence: row.get::<_, i64>(2)? as u64,
                chain_id: row.get(3)?,
                previous_chain_id: row.get(4)?,
                roster_epoch: row.get::<_, i64>(5)? as u64,
                payload_hash: row.get(6)?,
                created_at: row.get(7)?,
                ciphertext: row.get(8)?,
                reseal_material: row.get(9)?,
                recipient_order_stale: row.get(10)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// One currently queued outbox record, refreshed from durable state.
    pub fn pending_outgoing_record(&self, message_id: &str) -> Result<Option<PendingOutgoing>> {
        self.connection
            .query_row(
                "SELECT message_id, device_id, device_sequence, chain_id,
                        previous_chain_id, roster_epoch, payload_hash, created_at,
                        ciphertext, reseal_material, recipient_order_stale
                 FROM outbox
                 WHERE message_id = ?1 AND state IN ('queued', 'publishing')",
                params![message_id],
                |row| {
                    Ok(PendingOutgoing {
                        message_id: row.get(0)?,
                        device_id: row.get(1)?,
                        device_sequence: row.get::<_, i64>(2)? as u64,
                        chain_id: row.get(3)?,
                        previous_chain_id: row.get(4)?,
                        roster_epoch: row.get::<_, i64>(5)? as u64,
                        payload_hash: row.get(6)?,
                        created_at: row.get(7)?,
                        ciphertext: row.get(8)?,
                        reseal_material: row.get(9)?,
                        recipient_order_stale: row.get(10)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
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

    /// Every registered channel, in insertion order.
    pub fn channels(&self) -> Result<Vec<ChannelRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, transport_kind, transport_locator, local_name,
                    roster_epoch, control_sequence, sync_cursor, receive_cursor,
                    halted_reason
             FROM channel ORDER BY created_at, channel_id",
        )?;

        let rows = statement.query_map([], |row| {
            Ok(ChannelRecord {
                channel_id: row.get(0)?,
                transport_kind: row.get(1)?,
                transport_locator: row.get(2)?,
                local_name: row.get(3)?,
                roster_epoch: row.get::<_, i64>(4)? as u64,
                control_sequence: row.get::<_, i64>(5)? as u64,
                sync_cursor: row.get(6)?,
                receive_cursor: row.get(7)?,
                halted_reason: row.get(8)?,
            })
        })?;

        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Counts that `hrc status` reports for one channel (PRD section 29).
    pub fn channel_counts(&self, channel_id: &str) -> Result<ChannelCounts> {
        let outbox_pending = self.connection.query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE channel_id = ?1 AND state IN ('reserved', 'queued', 'publishing')",
            params![channel_id],
            |row| row.get::<_, i64>(0),
        )?;

        let unread = self.connection.query_row(
            "SELECT COUNT(*) FROM inbox WHERE channel_id = ?1 AND read_at IS NULL",
            params![channel_id],
            |row| row.get::<_, i64>(0),
        )?;

        let pending_approval = self.connection.query_row(
            "SELECT COUNT(*) FROM inbox
             WHERE channel_id = ?1
               AND disposition = 'quarantined'
               AND arrival_sequence IS NOT NULL",
            params![channel_id],
            |row| row.get::<_, i64>(0),
        )?;

        let last_error = self
            .connection
            .query_row(
                "SELECT last_error FROM outbox
                 WHERE channel_id = ?1 AND last_error IS NOT NULL
                 ORDER BY updated_at DESC LIMIT 1",
                params![channel_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();

        Ok(ChannelCounts {
            outbox_pending: outbox_pending as u64,
            unread: unread as u64,
            pending_approval: pending_approval as u64,
            last_error,
        })
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

    /// Audit entries in reverse chronological order.
    pub fn audit_entries(&self, since: Option<&str>) -> Result<Vec<AuditEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, message_id, action, detail, occurred_at
             FROM audit
             WHERE (?1 IS NULL OR occurred_at >= ?1)
             ORDER BY occurred_at DESC, id DESC",
        )?;
        let rows = statement.query_map(params![since], |row| {
            Ok(AuditEntry {
                channel_id: row.get(0)?,
                message_id: row.get(1)?,
                action: row.get(2)?,
                detail: row.get(3)?,
                occurred_at: row.get(4)?,
            })
        })?;
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

fn ensure_channel(transaction: &Transaction<'_>, channel_id: &str) -> Result<()> {
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

fn increment_device_sequence(
    transaction: &Transaction<'_>,
    channel_id: &str,
    device_id: &str,
) -> Result<(u64, Option<String>)> {
    transaction.execute(
        "INSERT INTO device_sequence (channel_id, device_id, last_sequence, last_chain_id)
         VALUES (?1, ?2, 0, NULL)
         ON CONFLICT (channel_id, device_id) DO NOTHING",
        params![channel_id, device_id],
    )?;

    transaction
        .query_row(
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
        )
        .map_err(Into::into)
}

fn set_last_allocated_chain(
    transaction: &Transaction<'_>,
    channel_id: &str,
    device_id: &str,
    chain_id: &str,
) -> Result<()> {
    transaction.execute(
        "UPDATE device_sequence SET last_chain_id = ?3
         WHERE channel_id = ?1 AND device_id = ?2",
        params![channel_id, device_id, chain_id],
    )?;
    Ok(())
}

fn latest_publishable_chain(
    transaction: &Transaction<'_>,
    channel_id: &str,
    device_id: &str,
    before_sequence: u64,
) -> Result<Option<String>> {
    transaction
        .query_row(
            "SELECT chain_id FROM outbox
             WHERE channel_id = ?1 AND device_id = ?2 AND device_sequence < ?3
               AND state IN ('queued', 'publishing', 'published')
             ORDER BY device_sequence DESC LIMIT 1",
            params![channel_id, device_id, before_sequence as i64],
            |row| row.get(0),
        )
        .optional()
        .map_err(Into::into)
}

fn recipient_predecessors_for(
    transaction: &Transaction<'_>,
    channel_id: &str,
    device_id: &str,
    before_sequence: u64,
    recipient_device_ids: &[String],
) -> Result<RecipientPredecessors> {
    let mut predecessors = BTreeMap::new();
    let mut statement = transaction.prepare(
        "SELECT outbox.chain_id
         FROM outbox
         JOIN outbox_recipient
           ON outbox_recipient.message_id = outbox.message_id
         WHERE outbox.channel_id = ?1
           AND outbox.device_id = ?2
           AND outbox.device_sequence < ?3
           AND outbox_recipient.device_id = ?4
           AND outbox.state IN ('queued', 'publishing', 'published')
         ORDER BY outbox.device_sequence DESC
         LIMIT 1",
    )?;

    for recipient in recipient_device_ids {
        let predecessor = statement
            .query_row(
                params![channel_id, device_id, before_sequence as i64, recipient],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        predecessors.insert(recipient.clone(), predecessor);
    }

    Ok(predecessors)
}

fn replace_recipient_facts(
    transaction: &Transaction<'_>,
    message_id: &str,
    recipient_device_ids: &[String],
) -> Result<()> {
    transaction.execute(
        "DELETE FROM outbox_recipient WHERE message_id = ?1",
        params![message_id],
    )?;
    for device_id in recipient_device_ids {
        transaction.execute(
            "INSERT INTO outbox_recipient (message_id, device_id) VALUES (?1, ?2)",
            params![message_id, device_id],
        )?;
    }
    Ok(())
}

fn outgoing_channel(transaction: &Transaction<'_>, message_id: &str) -> Result<String> {
    transaction
        .query_row(
            "SELECT channel_id FROM outbox WHERE message_id = ?1",
            params![message_id],
            |row| row.get(0),
        )
        .optional()?
        .ok_or_else(|| StorageError::UnknownMessage {
            message_id: message_id.to_owned(),
        })
}

fn pending_outgoing_in_transaction(
    transaction: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<PendingOutgoing>> {
    transaction
        .query_row(
            "SELECT message_id, device_id, device_sequence, chain_id,
                    previous_chain_id, roster_epoch, payload_hash, created_at,
                    ciphertext, reseal_material, recipient_order_stale
             FROM outbox
             WHERE message_id = ?1 AND state IN ('queued', 'publishing')",
            params![message_id],
            |row| {
                Ok(PendingOutgoing {
                    message_id: row.get(0)?,
                    device_id: row.get(1)?,
                    device_sequence: row.get::<_, i64>(2)? as u64,
                    chain_id: row.get(3)?,
                    previous_chain_id: row.get(4)?,
                    roster_epoch: row.get::<_, i64>(5)? as u64,
                    payload_hash: row.get(6)?,
                    created_at: row.get(7)?,
                    ciphertext: row.get(8)?,
                    reseal_material: row.get(9)?,
                    recipient_order_stale: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

/// A message taken back out of the hold table.
struct HeldMessage {
    message_id: String,
    sender_principal: String,
    device_sequence: u64,
    chain_id: String,
    previous_chain_id: Option<String>,
    kind: String,
    thread_id: Option<String>,
    in_reply_to: Option<String>,
    roster_epoch: u64,
    endpoint: Option<String>,
    ciphertext_bytes: u64,
    plaintext_bytes: u64,
    ciphertext_sha256: String,
    created_at: String,
    expires_at: Option<String>,
    attachment_count: u32,
    attachment_bytes: u64,
    prompt_request: bool,
    body: Vec<u8>,
    disposition: String,
}

/// Stores a verified context only after its owning inbox row exists.
///
/// The foreign key deliberately makes the message disposition its only
/// lifecycle: context cannot be approved, declined, or exposed separately.
fn store_inbound_context(
    transaction: &rusqlite::Transaction<'_>,
    message: &InboundMessage<'_>,
) -> Result<()> {
    if message.kind == "receipt" || message.expired {
        return Ok(());
    }
    let Some(context) = &message.context else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO inbound_context (message_id, package_id, digest, manifest)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (message_id) DO NOTHING",
        params![
            message.message_id,
            context.package_id,
            context.digest,
            context.manifest,
        ],
    )?;
    Ok(())
}

fn store_held_inbox(
    transaction: &Transaction<'_>,
    message: &InboundMessage<'_>,
    now: &str,
) -> Result<()> {
    if message.kind == "receipt" {
        return Ok(());
    }

    transaction.execute(
        "INSERT INTO inbox (
             message_id, channel_id, sender_principal, sender_device, kind, thread_id,
             in_reply_to, roster_epoch, endpoint, ciphertext_bytes, plaintext_bytes,
             created_at, expires_at, received_at, body, disposition,
             device_sequence, chain_id, previous_chain_id, ciphertext_sha256, arrival_sequence,
             attachment_count, attachment_bytes, prompt_request
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                   ?16, ?17, ?18, ?19, ?20, NULL, ?21, ?22, ?23)
         ON CONFLICT (message_id) DO NOTHING",
        params![
            message.message_id,
            message.channel_id,
            message.sender_principal,
            message.sender_device,
            message.kind,
            message.thread_id,
            message.in_reply_to,
            message.roster_epoch as i64,
            message.endpoint,
            message.ciphertext_bytes as i64,
            message.plaintext_bytes as i64,
            message.created_at,
            message.expires_at,
            now,
            (!message.expired).then_some(message.body),
            if message.expired {
                "expired"
            } else {
                "quarantined"
            },
            message.device_sequence as i64,
            message.chain_id,
            message.previous_chain_id,
            message.ciphertext_sha256,
            message.attachment_count,
            message.attachment_bytes as i64,
            message.prompt_request,
        ],
    )?;
    Ok(())
}

fn record_recipient_inbound(
    transaction: &Transaction<'_>,
    message: &InboundMessage<'_>,
    recipient_device: &str,
    now: &str,
) -> Result<InboundOutcome> {
    let predecessor = message.recipient_previous_chain_id;

    let competing_successor: Option<String> = transaction
        .query_row(
            "SELECT message_id FROM inbound_recipient_link
             WHERE channel_id = ?1 AND sender_device = ?2 AND recipient_device = ?3
               AND previous_chain_id IS ?4",
            params![
                message.channel_id,
                message.sender_device,
                recipient_device,
                predecessor
            ],
            |row| row.get(0),
        )
        .optional()?;
    if let Some(existing) = competing_successor {
        return Err(StorageError::InboundForked {
            sender_device: message.sender_device.to_owned(),
            device_sequence: message.device_sequence,
            existing,
            arriving: message.message_id.to_owned(),
        });
    }

    let head: Option<(u64, String)> = transaction
        .query_row(
            "SELECT last_sequence, last_chain_id FROM inbound_recipient_chain
             WHERE channel_id = ?1 AND sender_device = ?2 AND recipient_device = ?3",
            params![message.channel_id, message.sender_device, recipient_device],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)),
        )
        .optional()?;

    if let Some(previous) = predecessor
        && let Some(previous_sequence) = recipient_predecessor_sequence(
            transaction,
            message.channel_id,
            message.sender_device,
            recipient_device,
            previous,
        )?
        && message.device_sequence <= previous_sequence
    {
        return Err(StorageError::InboundForked {
            sender_device: message.sender_device.to_owned(),
            device_sequence: message.device_sequence,
            existing: previous.to_owned(),
            arriving: message.message_id.to_owned(),
        });
    }

    let ready = match (head.as_ref(), predecessor) {
        (Some((_, head)), Some(previous)) if head == previous => true,
        (Some(_), Some(previous)) => {
            if recipient_predecessor_accepted(
                transaction,
                message.channel_id,
                message.sender_device,
                recipient_device,
                previous,
                message.device_sequence,
                now,
            )? {
                return Err(StorageError::InboundForked {
                    sender_device: message.sender_device.to_owned(),
                    device_sequence: message.device_sequence,
                    existing: previous.to_owned(),
                    arriving: message.message_id.to_owned(),
                });
            }
            false
        }
        (Some((_, head)), None) => {
            return Err(StorageError::InboundForked {
                sender_device: message.sender_device.to_owned(),
                device_sequence: message.device_sequence,
                existing: head.to_owned(),
                arriving: message.message_id.to_owned(),
            });
        }
        (None, Some(previous)) => recipient_predecessor_accepted(
            transaction,
            message.channel_id,
            message.sender_device,
            recipient_device,
            previous,
            message.device_sequence,
            now,
        )?,
        (None, None) => {
            if legacy_sender_history_exists(transaction, message.channel_id, message.sender_device)?
            {
                return Err(StorageError::InboundForked {
                    sender_device: message.sender_device.to_owned(),
                    device_sequence: message.device_sequence,
                    existing: "legacy recipient history".into(),
                    arriving: message.message_id.to_owned(),
                });
            }
            true
        }
    };

    transaction.execute(
        "INSERT INTO inbound_recipient_link (
             message_id, channel_id, sender_device, recipient_device,
             device_sequence, chain_id, previous_chain_id, ciphertext_sha256,
             state, is_receipt, is_expired
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            message.message_id,
            message.channel_id,
            message.sender_device,
            recipient_device,
            message.device_sequence as i64,
            message.chain_id,
            predecessor,
            message.ciphertext_sha256,
            if ready { "accepted" } else { "held" },
            message.kind == "receipt",
            message.expired,
        ],
    )?;

    if !ready {
        store_held_inbox(transaction, message, now)?;
        store_inbound_context(transaction, message)?;
        return Ok(InboundOutcome::Held {
            waiting_for: predecessor.unwrap_or_default().to_owned(),
        });
    }

    let arrival_sequence = insert_accepted(transaction, message, now)?;
    store_inbound_context(transaction, message)?;

    let mut released = Vec::new();
    let mut link = message.chain_id.to_owned();
    let mut sequence = message.device_sequence;
    loop {
        let Some(held) = next_recipient_held(
            transaction,
            message.channel_id,
            message.sender_device,
            recipient_device,
            &link,
        )?
        else {
            break;
        };
        let RecipientHeldLink {
            message_id,
            device_sequence,
            chain_id,
            is_receipt,
            is_expired,
        } = held;
        if device_sequence <= sequence {
            return Err(StorageError::InboundForked {
                sender_device: message.sender_device.to_owned(),
                device_sequence,
                existing: link,
                arriving: message_id,
            });
        }

        let mut effective_expired = is_expired;
        if !is_receipt {
            let next = held_message_by_id(transaction, &message_id)?.ok_or_else(|| {
                StorageError::UnknownMessage {
                    message_id: message_id.clone(),
                }
            })?;
            effective_expired |= next.disposition == "expired";
            let body = next.body.clone();
            let waiting = InboundMessage {
                channel_id: message.channel_id,
                message_id: &next.message_id,
                sender_principal: &next.sender_principal,
                sender_device: message.sender_device,
                device_sequence: next.device_sequence,
                chain_id: &next.chain_id,
                previous_chain_id: next.previous_chain_id.as_deref(),
                recipient_device: Some(recipient_device),
                recipient_previous_chain_id: Some(&link),
                kind: &next.kind,
                thread_id: next.thread_id.as_deref(),
                in_reply_to: next.in_reply_to.as_deref(),
                roster_epoch: next.roster_epoch,
                endpoint: next.endpoint.as_deref(),
                ciphertext_bytes: next.ciphertext_bytes,
                plaintext_bytes: next.plaintext_bytes,
                ciphertext_sha256: &next.ciphertext_sha256,
                created_at: &next.created_at,
                expires_at: next.expires_at.as_deref(),
                expired: effective_expired,
                attachment_count: next.attachment_count,
                attachment_bytes: next.attachment_bytes,
                prompt_request: next.prompt_request,
                body: &body,
                ciphertext: &[],
                context: None,
            };
            insert_accepted(transaction, &waiting, now)?;
            if !effective_expired {
                released.push(message_id.clone());
            }
        }

        transaction.execute(
            "UPDATE inbound_recipient_link
             SET state = 'accepted', is_expired = ?2 WHERE message_id = ?1",
            params![message_id, effective_expired],
        )?;
        sequence = device_sequence;
        link = chain_id;
    }

    transaction.execute(
        "INSERT INTO inbound_recipient_chain (
             channel_id, sender_device, recipient_device, last_sequence, last_chain_id
         ) VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT (channel_id, sender_device, recipient_device)
         DO UPDATE SET last_sequence = excluded.last_sequence,
                       last_chain_id = excluded.last_chain_id",
        params![
            message.channel_id,
            message.sender_device,
            recipient_device,
            sequence as i64,
            link,
        ],
    )?;

    Ok(match (arrival_sequence, message.expired) {
        (_, true) => InboundOutcome::ExpiredAccepted { released },
        (Some(arrival_sequence), false) => InboundOutcome::Accepted {
            arrival_sequence,
            released,
        },
        (None, false) => InboundOutcome::ReceiptAccepted { released },
    })
}

fn recipient_predecessor_accepted(
    transaction: &Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
    recipient_device: &str,
    chain_id: &str,
    successor_sequence: u64,
    now: &str,
) -> Result<bool> {
    let accepted = transaction
        .query_row(
            "SELECT 1
             WHERE EXISTS (
                 SELECT 1 FROM inbound_recipient_link
                 WHERE channel_id = ?1 AND sender_device = ?2
                   AND recipient_device = ?3 AND chain_id = ?4
                   AND state = 'accepted'
             )
             OR EXISTS (
                 SELECT 1 FROM inbound_chain
                 WHERE channel_id = ?1 AND sender_device = ?2
                   AND last_chain_id = ?4
             )",
            params![channel_id, sender_device, recipient_device, chain_id],
            |_| Ok(true),
        )
        .optional()
        .map(|found| found.unwrap_or(false))
        .map_err(StorageError::from)?;
    if accepted {
        return Ok(true);
    }

    adopt_legacy_held_predecessor(
        transaction,
        channel_id,
        sender_device,
        recipient_device,
        chain_id,
        successor_sequence,
        now,
    )
}

fn adopt_legacy_held_predecessor(
    transaction: &Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
    recipient_device: &str,
    chain_id: &str,
    successor_sequence: u64,
    now: &str,
) -> Result<bool> {
    let mut held_chain = Vec::new();
    let mut current_chain_id = chain_id.to_owned();
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(current_chain_id.clone()) {
            return Err(StorageError::InboundForked {
                sender_device: sender_device.to_owned(),
                device_sequence: successor_sequence,
                existing: current_chain_id,
                arriving: chain_id.to_owned(),
            });
        }
        let held: Option<LegacyHeldPredecessor> = transaction
            .query_row(
                "SELECT inbound_hold.message_id, inbound_hold.device_sequence,
                    inbound_hold.chain_id, inbound_hold.previous_chain_id,
                    inbound_hold.ciphertext_sha256,
                    EXISTS (
                        SELECT 1 FROM inbound_receipt_link
                        WHERE inbound_receipt_link.message_id = inbound_hold.message_id
                    ),
                    COALESCE((
                        SELECT disposition = 'expired' FROM inbox
                        WHERE inbox.message_id = inbound_hold.message_id
                    ), 0)
             FROM inbound_hold
             WHERE inbound_hold.channel_id = ?1
               AND inbound_hold.sender_device = ?2
               AND inbound_hold.chain_id = ?3",
                params![channel_id, sender_device, current_chain_id],
                |row| {
                    Ok(LegacyHeldPredecessor {
                        message_id: row.get(0)?,
                        device_sequence: row.get::<_, i64>(1)? as u64,
                        chain_id: row.get(2)?,
                        previous_chain_id: row.get(3)?,
                        ciphertext_sha256: row.get(4)?,
                        is_receipt: row.get(5)?,
                        is_expired: row.get(6)?,
                    })
                },
            )
            .optional()?;
        let Some(held) = held else {
            break;
        };
        current_chain_id = match &held.previous_chain_id {
            Some(previous) => previous.clone(),
            None => {
                held_chain.push(held);
                break;
            }
        };
        held_chain.push(held);
    }
    if held_chain.is_empty() {
        return Ok(false);
    }
    held_chain.reverse();

    let legacy_head: Option<(u64, String)> = transaction
        .query_row(
            "SELECT last_sequence, last_chain_id FROM inbound_chain
             WHERE channel_id = ?1 AND sender_device = ?2",
            params![channel_id, sender_device],
            |row| Ok((row.get::<_, i64>(0)? as u64, row.get(1)?)),
        )
        .optional()?;
    let oldest = &held_chain[0];
    if oldest.device_sequence == 1 && oldest.previous_chain_id.is_some() {
        return Err(StorageError::InboundForked {
            sender_device: sender_device.to_owned(),
            device_sequence: oldest.device_sequence,
            existing: oldest.previous_chain_id.clone().unwrap_or_default(),
            arriving: oldest.message_id.clone(),
        });
    }
    if let Some((head_sequence, head_chain_id)) = legacy_head
        && oldest.device_sequence <= head_sequence
    {
        return Err(StorageError::InboundForked {
            sender_device: sender_device.to_owned(),
            device_sequence: oldest.device_sequence,
            existing: head_chain_id,
            arriving: oldest.message_id.clone(),
        });
    }
    for pair in held_chain.windows(2) {
        if pair[1].previous_chain_id.as_deref() != Some(pair[0].chain_id.as_str())
            || pair[1].device_sequence <= pair[0].device_sequence
        {
            return Err(StorageError::InboundForked {
                sender_device: sender_device.to_owned(),
                device_sequence: pair[1].device_sequence,
                existing: pair[0].chain_id.clone(),
                arriving: pair[1].message_id.clone(),
            });
        }
    }
    if held_chain.last().unwrap().device_sequence >= successor_sequence {
        return Err(StorageError::InboundForked {
            sender_device: sender_device.to_owned(),
            device_sequence: successor_sequence,
            existing: held_chain.last().unwrap().chain_id.clone(),
            arriving: chain_id.to_owned(),
        });
    }

    let mut recipient_previous_chain_id = None;
    for held in held_chain {
        transaction.execute(
            "INSERT INTO inbound_recipient_link (
                 message_id, channel_id, sender_device, recipient_device,
                 device_sequence, chain_id, previous_chain_id, ciphertext_sha256,
                 state, is_receipt, is_expired
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'accepted', ?9, ?10)",
            params![
                held.message_id,
                channel_id,
                sender_device,
                recipient_device,
                held.device_sequence as i64,
                held.chain_id,
                recipient_previous_chain_id,
                held.ciphertext_sha256,
                held.is_receipt,
                held.is_expired,
            ],
        )?;
        recipient_previous_chain_id = Some(held.chain_id.clone());

        if !held.is_receipt {
            let arrival_sequence: u64 = transaction.query_row(
                "SELECT COALESCE(MAX(arrival_sequence), 0) + 1
                 FROM inbox WHERE channel_id = ?1",
                params![channel_id],
                |row| row.get::<_, i64>(0),
            )? as u64;
            transaction.execute(
                "UPDATE inbox SET arrival_sequence = COALESCE(arrival_sequence, ?2),
                                  received_at = ?3
                 WHERE message_id = ?1",
                params![held.message_id, arrival_sequence as i64, now],
            )?;
        }
        transaction.execute(
            "DELETE FROM inbound_receipt_link WHERE message_id = ?1",
            params![held.message_id],
        )?;
        transaction.execute(
            "DELETE FROM inbound_hold WHERE message_id = ?1",
            params![held.message_id],
        )?;
    }
    Ok(true)
}

struct LegacyHeldPredecessor {
    message_id: String,
    device_sequence: u64,
    chain_id: String,
    previous_chain_id: Option<String>,
    ciphertext_sha256: String,
    is_receipt: bool,
    is_expired: bool,
}

fn recipient_predecessor_sequence(
    transaction: &Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
    recipient_device: &str,
    chain_id: &str,
) -> Result<Option<u64>> {
    transaction
        .query_row(
            "SELECT device_sequence FROM inbound_recipient_link
             WHERE channel_id = ?1 AND sender_device = ?2
               AND recipient_device = ?3 AND chain_id = ?4
             UNION ALL
             SELECT last_sequence FROM inbound_chain
             WHERE channel_id = ?1 AND sender_device = ?2 AND last_chain_id = ?4",
            params![channel_id, sender_device, recipient_device, chain_id],
            |row| Ok(row.get::<_, i64>(0)? as u64),
        )
        .optional()
        .map_err(Into::into)
}

fn legacy_sender_history_exists(
    transaction: &Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
) -> Result<bool> {
    transaction
        .query_row(
            "SELECT 1
             WHERE EXISTS (
                 SELECT 1 FROM inbox
                 WHERE channel_id = ?1 AND sender_device = ?2
                   AND NOT EXISTS (
                       SELECT 1 FROM inbound_recipient_link
                       WHERE inbound_recipient_link.message_id = inbox.message_id
                   )
             )
             OR EXISTS (
                 SELECT 1 FROM inbound_receipt_link
                 WHERE channel_id = ?1 AND sender_device = ?2
                   AND NOT EXISTS (
                       SELECT 1 FROM inbound_recipient_link
                       WHERE inbound_recipient_link.message_id =
                             inbound_receipt_link.message_id
                   )
             )",
            params![channel_id, sender_device],
            |_| Ok(true),
        )
        .optional()
        .map(|found| found.unwrap_or(false))
        .map_err(Into::into)
}

fn next_recipient_held(
    transaction: &Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
    recipient_device: &str,
    predecessor: &str,
) -> Result<Option<RecipientHeldLink>> {
    transaction
        .query_row(
            "SELECT message_id, device_sequence, chain_id, is_receipt, is_expired
             FROM inbound_recipient_link
             WHERE channel_id = ?1 AND sender_device = ?2 AND recipient_device = ?3
               AND state = 'held' AND previous_chain_id = ?4",
            params![channel_id, sender_device, recipient_device, predecessor],
            |row| {
                Ok(RecipientHeldLink {
                    message_id: row.get(0)?,
                    device_sequence: row.get::<_, i64>(1)? as u64,
                    chain_id: row.get(2)?,
                    is_receipt: row.get(3)?,
                    is_expired: row.get(4)?,
                })
            },
        )
        .optional()
        .map_err(Into::into)
}

struct RecipientHeldLink {
    message_id: String,
    device_sequence: u64,
    chain_id: String,
    is_receipt: bool,
    is_expired: bool,
}

fn held_message_by_id(
    transaction: &Transaction<'_>,
    message_id: &str,
) -> Result<Option<HeldMessage>> {
    transaction
        .query_row(
            "SELECT inbox.message_id, inbox.sender_principal, inbox.device_sequence,
                    inbox.chain_id, inbox.previous_chain_id, inbox.kind, inbox.thread_id,
                    inbox.in_reply_to, inbox.roster_epoch, inbox.endpoint,
                    inbox.ciphertext_bytes, inbox.plaintext_bytes, inbox.ciphertext_sha256,
                    inbox.created_at, inbox.expires_at, inbox.attachment_count,
                    inbox.attachment_bytes, inbox.prompt_request, inbox.body,
                    inbox.disposition
             FROM inbox
             WHERE inbox.message_id = ?1 AND inbox.arrival_sequence IS NULL",
            params![message_id],
            held_message_from_row,
        )
        .optional()
        .map_err(Into::into)
}

fn held_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeldMessage> {
    Ok(HeldMessage {
        message_id: row.get(0)?,
        sender_principal: row.get(1)?,
        device_sequence: row.get::<_, i64>(2)? as u64,
        chain_id: row.get(3)?,
        previous_chain_id: row.get(4)?,
        kind: row.get(5)?,
        thread_id: row.get(6)?,
        in_reply_to: row.get(7)?,
        roster_epoch: row.get::<_, i64>(8)? as u64,
        endpoint: row.get(9)?,
        ciphertext_bytes: row.get::<_, i64>(10)? as u64,
        plaintext_bytes: row.get::<_, i64>(11)? as u64,
        ciphertext_sha256: row.get(12)?,
        created_at: row.get(13)?,
        expires_at: row.get(14)?,
        attachment_count: row.get::<_, i64>(15)? as u32,
        attachment_bytes: row.get::<_, i64>(16)? as u64,
        prompt_request: row.get(17)?,
        body: row.get::<_, Option<Vec<u8>>>(18)?.unwrap_or_default(),
        disposition: row.get(19)?,
    })
}

/// Parks a message whose predecessor has not arrived.
fn hold(
    transaction: &rusqlite::Transaction<'_>,
    message: &InboundMessage<'_>,
    now: &str,
) -> Result<InboundOutcome> {
    transaction.execute(
        "INSERT INTO inbound_hold (
             message_id, channel_id, sender_device, device_sequence, chain_id,
             previous_chain_id, ciphertext_sha256, ciphertext, held_at
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
         ON CONFLICT (message_id) DO NOTHING",
        params![
            message.message_id,
            message.channel_id,
            message.sender_device,
            message.device_sequence as i64,
            message.chain_id,
            message.previous_chain_id,
            message.ciphertext_sha256,
            message.ciphertext,
            now,
        ],
    )?;

    if message.kind == "receipt" {
        return Ok(InboundOutcome::Held {
            waiting_for: message.previous_chain_id.unwrap_or_default().to_owned(),
        });
    }

    // The held row records everything needed to reconsider it later. The
    // decrypted body travels with it so releasing does not have to decrypt
    // again — and it is stored in the same quarantined state, never exposed.
    store_held_inbox(transaction, message, now)?;

    Ok(InboundOutcome::Held {
        waiting_for: message.previous_chain_id.unwrap_or_default().to_owned(),
    })
}

/// Writes an accepted message and gives it its arrival number.
fn insert_accepted(
    transaction: &rusqlite::Transaction<'_>,
    message: &InboundMessage<'_>,
    now: &str,
) -> Result<Option<u64>> {
    if message.kind == "receipt" {
        transaction.execute(
            "DELETE FROM inbound_hold WHERE message_id = ?1",
            params![message.message_id],
        )?;
        return Ok(None);
    }

    let arrival_sequence: u64 = transaction.query_row(
        "SELECT COALESCE(MAX(arrival_sequence), 0) + 1 FROM inbox WHERE channel_id = ?1",
        params![message.channel_id],
        |row| row.get::<_, i64>(0),
    )? as u64;

    // A held message already has a row; accepting it fills in the arrival
    // number rather than inserting a second one.
    let updated = transaction.execute(
        "UPDATE inbox SET arrival_sequence = ?2, received_at = ?3
         WHERE message_id = ?1 AND arrival_sequence IS NULL",
        params![message.message_id, arrival_sequence as i64, now],
    )?;

    if updated == 0 {
        transaction.execute(
            "INSERT INTO inbox (
                 message_id, channel_id, sender_principal, sender_device, kind, thread_id,
                 in_reply_to, roster_epoch, endpoint, ciphertext_bytes, plaintext_bytes,
                 created_at, expires_at, received_at, body, disposition,
                 device_sequence, chain_id, previous_chain_id, ciphertext_sha256, arrival_sequence,
                 attachment_count, attachment_bytes, prompt_request
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                       ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24)",
            params![
                message.message_id,
                message.channel_id,
                message.sender_principal,
                message.sender_device,
                message.kind,
                message.thread_id,
                message.in_reply_to,
                message.roster_epoch as i64,
                message.endpoint,
                message.ciphertext_bytes as i64,
                message.plaintext_bytes as i64,
                message.created_at,
                message.expires_at,
                now,
                (!message.expired).then_some(message.body),
                if message.expired {
                    "expired"
                } else {
                    "quarantined"
                },
                message.device_sequence as i64,
                message.chain_id,
                message.previous_chain_id,
                message.ciphertext_sha256,
                arrival_sequence as i64,
                message.attachment_count,
                message.attachment_bytes as i64,
                message.prompt_request,
            ],
        )?;
    }

    transaction.execute(
        "DELETE FROM inbound_hold WHERE message_id = ?1",
        params![message.message_id],
    )?;

    Ok(Some(arrival_sequence))
}

/// Takes the message waiting on `link`, if one is held.
fn take_held(
    transaction: &rusqlite::Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
    link: &str,
) -> Result<Option<HeldMessage>> {
    transaction
        .query_row(
            "SELECT hold.message_id, inbox.sender_principal, hold.device_sequence,
                    hold.chain_id, hold.previous_chain_id, inbox.kind, inbox.thread_id,
                    inbox.in_reply_to, inbox.roster_epoch, inbox.endpoint,
                    inbox.ciphertext_bytes, inbox.plaintext_bytes, hold.ciphertext_sha256,
                    inbox.created_at, inbox.expires_at, inbox.attachment_count,
                    inbox.attachment_bytes, inbox.prompt_request, inbox.body,
                    inbox.disposition
             FROM inbound_hold AS hold
             JOIN inbox ON inbox.message_id = hold.message_id
             WHERE hold.channel_id = ?1 AND hold.sender_device = ?2
               AND hold.previous_chain_id = ?3",
            params![channel_id, sender_device, link],
            held_message_from_row,
        )
        .optional()
        .map_err(Into::into)
}

/// Takes a receipt waiting on `link` without consulting the inbox.
fn take_held_receipt(
    transaction: &rusqlite::Transaction<'_>,
    channel_id: &str,
    sender_device: &str,
    link: &str,
) -> Result<Option<(String, u64, String)>> {
    transaction
        .query_row(
            "SELECT receipt.message_id, receipt.device_sequence, receipt.chain_id
             FROM inbound_receipt_link AS receipt
             JOIN inbound_hold AS hold ON hold.message_id = receipt.message_id
             WHERE receipt.channel_id = ?1 AND receipt.sender_device = ?2
               AND receipt.previous_chain_id = ?3",
            params![channel_id, sender_device, link],
            |row| Ok((row.get(0)?, row.get::<_, i64>(1)? as u64, row.get(2)?)),
        )
        .optional()
        .map_err(Into::into)
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
