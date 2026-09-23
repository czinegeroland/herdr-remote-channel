//! The outbox: allocating, composing, resealing and publishing outgoing messages.

use super::*;

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

pub(crate) fn increment_device_sequence(
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

pub(crate) fn set_last_allocated_chain(
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

pub(crate) fn latest_publishable_chain(
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

pub(crate) fn recipient_predecessors_for(
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

pub(crate) fn replace_recipient_facts(
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

pub(crate) fn outgoing_channel(transaction: &Transaction<'_>, message_id: &str) -> Result<String> {
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

pub(crate) fn pending_outgoing_in_transaction(
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

impl Database {
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
        in_reply_to: Option<&str>,
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
                     reseal_material, thread_id, kind, in_reply_to, state,
                     created_at, updated_at
                 ) VALUES (
                     ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13,
                     'queued', ?14, ?14
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
                    in_reply_to,
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
}
