//! The inbox: every read of what arrived, and the expiry sweep. Recording an
//! arrival is in `ordering`, because whether it is released or held is decided
//! there.

use super::*;

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

/// One message's place in a thread, by this installation's own clock.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadPlace {
    /// The message.
    pub message_id: String,
    /// Whether this installation sent it.
    pub outbound: bool,
}

/// Questions with no answer, counted in both directions.
///
/// Two numbers rather than one, because they ask different things of a
/// person. One is work they owe somebody else; the other is work somebody
/// else owes them, and only the first is theirs to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unanswered {
    /// Questions asked of this installation that it has not answered.
    pub owed: usize,
    /// Questions this installation asked that nobody has answered.
    pub awaiting: usize,
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

impl Database {
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

    /// One thread's entries, in local arrival order.
    ///
    /// Ordering is by arrival rather than by the sender's `created_at`,
    /// which is a value the sender chooses: a backdated timestamp would
    /// otherwise let a remote peer place its message anywhere in someone
    /// else's reading of the conversation.
    pub fn thread_entries(&self, channel_id: &str, thread_id: &str) -> Result<Vec<InboxEntry>> {
        self.query_inbox(channel_id, Some(thread_id))
    }

    /// The messages of one thread in the order this installation saw them.
    ///
    /// Inbound messages by when they arrived here and outbound ones by when
    /// they were written here -- both this machine's clock. Never by the
    /// sender's `createdAt`: a remote peer chooses that, and ordering by it
    /// would let them place a message anywhere in someone else's reading of
    /// the conversation (section 18.2, decision DEC-042). Ties within one
    /// second fall back to arrival and device sequence, which are also
    /// local.
    pub fn thread_order(&self, channel_id: &str, thread_id: &str) -> Result<Vec<ThreadPlace>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, 0 AS outbound, received_at AS local_at,
                    arrival_sequence AS sequence
             FROM inbox
             WHERE channel_id = ?1 AND thread_id = ?2 AND arrival_sequence IS NOT NULL
             UNION ALL
             SELECT message_id, 1, created_at, device_sequence
             FROM outbox
             WHERE channel_id = ?1 AND thread_id = ?2
             ORDER BY local_at, outbound, sequence",
        )?;

        let rows = statement.query_map(params![channel_id, thread_id], |row| {
            Ok(ThreadPlace {
                message_id: row.get(0)?,
                outbound: row.get::<_, i64>(1)? == 1,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
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

    /// Questions nobody has answered, in both directions.
    ///
    /// The largest measured failure mode for coding agents working together
    /// is a question that goes unanswered, which breaks the decision loop it
    /// was asked inside. Threads and receipts already existed here; what did
    /// not was any way to see that a question had fallen on the floor.
    ///
    /// "Answered" means a message that names the question as what it
    /// answers. A later message in the same thread is deliberately not
    /// enough: a thread accumulates notes, and treating any of them as an
    /// answer would quietly close a question nobody addressed.
    ///
    /// Both halves exclude what can no longer be acted on. An expired or
    /// declined question is not outstanding -- it is finished, and listing
    /// it would make the count something people learn to ignore.
    pub fn unanswered(&self, channel_id: &str) -> Result<Unanswered> {
        // Asked of this installation: a question that arrived, still stands,
        // and that nothing this installation sent names as its target.
        let mut theirs = self.connection.prepare(
            "SELECT COUNT(*) FROM inbox
             WHERE channel_id = ?1
               AND kind = 'question'
               AND disposition NOT IN ('declined', 'expired')
               AND NOT EXISTS (
                   SELECT 1 FROM outbox
                   WHERE outbox.channel_id = inbox.channel_id
                     AND outbox.in_reply_to = inbox.message_id
               )",
        )?;
        let owed: i64 = theirs.query_row(params![channel_id], |row| row.get(0))?;

        // Asked by this installation: a question that was published and that
        // nothing which arrived names as its target. A message still queued
        // has not reached anyone, so nobody has had the chance to answer it
        // and it is not outstanding on them.
        let mut ours = self.connection.prepare(
            "SELECT COUNT(*) FROM outbox
             WHERE channel_id = ?1
               AND kind = 'question'
               AND state = 'published'
               AND NOT EXISTS (
                   SELECT 1 FROM inbox
                   WHERE inbox.channel_id = outbox.channel_id
                     AND inbox.in_reply_to = outbox.message_id
               )",
        )?;
        let awaiting: i64 = ours.query_row(params![channel_id], |row| row.get(0))?;

        Ok(Unanswered {
            owed: owed as usize,
            awaiting: awaiting as usize,
        })
    }
}
