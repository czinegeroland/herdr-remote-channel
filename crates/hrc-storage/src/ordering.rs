//! Held messages and per-sender, per-recipient ordering (PRD section 17).
//!
//! Everything that decides whether an arrival is released now or held for a
//! predecessor lives here, because it is one consistency argument and
//! splitting it would put half of that argument in each file.

use super::*;

/// A message taken back out of the hold table.
pub(crate) struct HeldMessage {
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

pub(crate) fn store_held_inbox(
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

pub(crate) fn record_recipient_inbound(
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

pub(crate) fn recipient_predecessor_accepted(
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

pub(crate) fn adopt_legacy_held_predecessor(
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

pub(crate) struct LegacyHeldPredecessor {
    message_id: String,
    device_sequence: u64,
    chain_id: String,
    previous_chain_id: Option<String>,
    ciphertext_sha256: String,
    is_receipt: bool,
    is_expired: bool,
}

pub(crate) fn recipient_predecessor_sequence(
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

pub(crate) fn legacy_sender_history_exists(
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

pub(crate) fn next_recipient_held(
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

pub(crate) struct RecipientHeldLink {
    message_id: String,
    device_sequence: u64,
    chain_id: String,
    is_receipt: bool,
    is_expired: bool,
}

pub(crate) fn held_message_by_id(
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

pub(crate) fn held_message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<HeldMessage> {
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
pub(crate) fn hold(
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
pub(crate) fn insert_accepted(
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
pub(crate) fn take_held(
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
pub(crate) fn take_held_receipt(
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

impl Database {
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
}
