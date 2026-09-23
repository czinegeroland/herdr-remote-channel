//! Synchronization: receiving, publishing, receipts, and the cursors that
//! keep a history honest (PRD section 17).

use super::*;

/// Opens a fresh transport and device identity for the receive pass.
///
/// Separate from the synchronization pass because receiving needs the key
/// store, and a channel can be synchronized by a process that has no
/// passphrase. A locked installation can still validate and advance public
/// state; queued ciphertext that became stale stays deferred until the key
/// store can be unlocked.
pub(super) fn receive_for(
    context: &Context,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<Vec<String>> {
    let identity = match context.passphrase.clone() {
        Some(passphrase) => {
            let store = PassphraseStore::open(context.paths.keys(), passphrase)?;
            if store.contains(DEVICE_KEY_NAME)? {
                let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
                Some(device.device_identity()?)
            } else {
                None
            }
        }
        None => None,
    };

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;

    let mut database = Database::open(context.paths.database())?;
    let received = receive_messages(&transport, &mut database, channel, identity.as_ref(), now)?;

    Ok(received)
}

/// Tells each sender that their messages arrived (PRD section 18.2).
///
/// Pending deliveries are recovered from durable inbox and audit records,
/// then batched within the protocol's limit. A failed publication must not
/// lose the obligation to report an already accepted message.
///
/// Only `delivered` is reported here. `read` is a different claim — that a
/// human opened it — and whether to make it the default is still open
/// question OQ-007, so nothing here decides it.
///
/// Receipts never generate receipts: an arriving one is recorded and skipped
/// before it reaches the inbox, so it is never among the messages this
/// reports on.
pub(super) fn publish_delivery_receipts(
    context: &Context,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<()> {
    let database = Database::open(context.paths.database())?;
    let reported: std::collections::HashSet<String> = database
        .audit_entries(None)?
        .into_iter()
        .filter(|entry| {
            entry.channel_id.as_deref() == Some(&channel.channel_id)
                && entry.action == "delivery_receipt_queued"
        })
        .filter_map(|entry| entry.message_id)
        .collect();

    let by_sender =
        delivery_receipt_obligations(database.inbox_entries(&channel.channel_id)?, &reported);

    for ((sender, _sender_device), message_ids) in by_sender {
        for batch in message_ids.chunks(hrc_protocol::receipt::MAX_REFERENCED_MESSAGES) {
            let body = hrc_core::receipt::build_receipt(
                batch.iter().cloned(),
                hrc_protocol::ReceiptState::Delivered,
                now,
            )?;
            let body = canonical::to_canonical_json(&body)?;
            compose_body(
                context,
                hrc_protocol::MessageKind::Receipt,
                &sender,
                canonical::from_json_str(&body)?,
                None,
                None,
            )?;

            // A crash before these markers may repeat a report, which the
            // receiver deduplicates. Marking before queueing could lose it.
            for message_id in batch {
                database.append_audit(
                    Some(&channel.channel_id),
                    Some(message_id),
                    "delivery_receipt_queued",
                    None,
                    None,
                    now,
                )?;
            }
        }
    }

    Ok(())
}

pub(super) fn delivery_receipt_obligations(
    entries: Vec<hrc_storage::InboxEntry>,
    reported: &std::collections::HashSet<String>,
) -> std::collections::BTreeMap<(String, String), Vec<String>> {
    let mut by_sender = std::collections::BTreeMap::new();
    for entry in entries {
        if entry.disposition != "expired" && !reported.contains(&entry.message_id) {
            by_sender
                .entry((entry.sender_principal, entry.sender_device))
                .or_insert_with(Vec::new)
                .push(entry.message_id);
        }
    }
    by_sender
}

/// Decrypts and records every message published since the last receive pass.
///
/// The walk replays control and message objects together, in publication
/// order, applying control entries to the roster as it goes. That ordering is
/// the point: a message must be opened against the roster as it stood when
/// the message was introduced, not as it stands now. Using the current epoch
/// would reject a message that was legitimate when it was sent and whose
/// sender has since been revoked — and accepting one under the wrong epoch
/// would do the opposite.
pub(super) fn receive_messages(
    transport: &GitTransport,
    database: &mut Database,
    channel: &hrc_storage::ChannelRecord,
    identity: Option<&hrc_crypto::DeviceIdentity>,
    now: &str,
) -> Result<Vec<String>> {
    let mut roster: Option<Roster> = None;
    let mut received = Vec::new();
    // Rebuild the roster from genesis on every receive pass. The receive
    // cursor decides whether a pass is necessary, but resuming the transport
    // walk at an arbitrary data commit would leave `roster` empty until the
    // next control object and silently skip intervening ciphertext.
    let mut cursor = None;

    loop {
        let page = transport.fetch(cursor.as_deref(), 100)?;

        for publication in &page.publications {
            for object in &publication.objects {
                let bytes = transport.get_object(&object.name, &object.sha256)?;

                match object.class {
                    ObjectClass::Control => {
                        let text = std::str::from_utf8(&bytes).map_err(|_| {
                            hrc_transport::TransportError::InvalidPublication {
                                reason: format!("control object {} is not UTF-8", object.name),
                            }
                        })?;

                        match &mut roster {
                            None => {
                                let genesis: SignedObject<GenesisPayload> =
                                    canonical::from_json_str(text)?;
                                roster = Some(Roster::from_genesis(&genesis)?);
                            }
                            Some(roster) => {
                                let entry: SignedObject<ControlEntryPayload> =
                                    canonical::from_json_str(text)?;
                                roster.apply(&entry)?;
                            }
                        }
                    }

                    ObjectClass::Message => {
                        let Some(identity) = identity else {
                            continue;
                        };
                        let Some(roster) = roster.as_ref() else {
                            continue;
                        };

                        // A message this installation cannot decrypt is not
                        // an error: it was addressed to someone else, and a
                        // channel member seeing ciphertext they are not a
                        // recipient of is the ordinary case.
                        let Ok(opened) = hrc_core::message::open_for_ordering(
                            roster,
                            identity,
                            &bytes,
                            roster.epoch(),
                            now,
                        ) else {
                            continue;
                        };
                        let expired = opened.is_expired();
                        let opened = opened.message();
                        let local_recipient = roster
                            .devices()
                            .find(|device| {
                                device.encryption_recipient == identity.recipient().to_string()
                            })
                            .map(|device| device.device_id.as_str());
                        let recipient_previous_chain_id = local_recipient
                            .and_then(|device_id| opened.envelope.recipient_predecessor(device_id))
                            .flatten();
                        let recipient_device = opened
                            .envelope
                            .recipient_previous_chain_ids
                            .as_ref()
                            .and(local_recipient);

                        let is_receipt =
                            opened.envelope.kind == hrc_protocol::MessageKind::Receipt.as_str();
                        let received_context = if is_receipt {
                            Ok(None)
                        } else {
                            ContextPackage::from_message_body(&opened.envelope.body)
                        };
                        let malformed_context =
                            received_context.as_ref().err().map(ToString::to_string);
                        let received_context = received_context.ok().flatten();
                        let context_manifest = received_context
                            .as_ref()
                            .map(|(package, _)| canonical::to_canonical_bytes(package))
                            .transpose()?;
                        let context = received_context
                            .as_ref()
                            .zip(context_manifest.as_deref())
                            .map(
                                |((package, digest), manifest)| hrc_storage::InboundContext {
                                    package_id: &package.id,
                                    digest,
                                    manifest,
                                },
                            );
                        // For a context-bearing message the canonical body is
                        // the pending content. It owns the context lifecycle:
                        // trusted approval reveals/delivers this exact body,
                        // while agent-safe views never receive it.
                        let body = if received_context.is_some()
                            || malformed_context.is_some()
                            || !matches!(
                                opened.envelope.kind.as_str(),
                                "note" | "question" | "answer"
                            ) {
                            canonical::to_canonical_json(&opened.envelope.body)?
                        } else {
                            opened
                                .envelope
                                .body
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned()
                        };
                        let plaintext_bytes =
                            canonical::to_canonical_bytes(&opened.envelope.body)?.len() as u64;

                        let kind = hrc_protocol::MessageKind::parse(&opened.envelope.kind)
                            .map(|kind| kind.as_str())
                            .unwrap_or("unsupported");

                        let chain_id = hrc_storage::chain_id_for(
                            &channel.channel_id,
                            &opened.sender_device,
                            opened.envelope.device_sequence,
                            &opened.envelope.message_id,
                        )?;

                        let outcome = database.record_inbound(
                            &hrc_storage::InboundMessage {
                                channel_id: &channel.channel_id,
                                message_id: &opened.envelope.message_id,
                                sender_principal: &opened.sender_principal,
                                sender_device: &opened.sender_device,
                                device_sequence: opened.envelope.device_sequence,
                                chain_id: &chain_id,
                                previous_chain_id: opened.envelope.previous_chain_id.as_deref(),
                                recipient_device,
                                recipient_previous_chain_id,
                                kind,
                                thread_id: Some(&opened.envelope.thread_id),
                                in_reply_to: opened.envelope.in_reply_to.as_deref(),
                                roster_epoch: opened.envelope.roster_epoch,
                                endpoint: opened.envelope.to.endpoint.as_deref(),
                                ciphertext_bytes: bytes.len() as u64,
                                plaintext_bytes,
                                ciphertext_sha256: &opened.ciphertext_sha256,
                                created_at: &opened.envelope.created_at,
                                expires_at: opened.envelope.expires_at.as_deref(),
                                expired,
                                attachment_count: u32::try_from(opened.envelope.attachments.len())
                                    .unwrap_or(u32::MAX),
                                attachment_bytes: opened.envelope.attachments.iter().fold(
                                    0u64,
                                    |total, attachment| {
                                        total.saturating_add(attachment.ciphertext_bytes)
                                    },
                                ),
                                prompt_request: opened.envelope.requested_capability.as_deref()
                                    == Some(hrc_core::gate::PROMPT_CAPABILITY),
                                body: body.as_bytes(),
                                ciphertext: &bytes,
                                context,
                            },
                            now,
                        )?;

                        match outcome {
                            hrc_storage::InboundOutcome::Accepted { released, .. } => {
                                received.push(opened.envelope.message_id.clone());
                                received.extend(released);
                            }
                            hrc_storage::InboundOutcome::ReceiptAccepted { released } => {
                                received.extend(released);
                            }
                            hrc_storage::InboundOutcome::ExpiredAccepted { released } => {
                                received.extend(released);
                            }
                            _ => {}
                        }
                        if is_receipt && !expired {
                            // A receipt's authenticated link must advance the
                            // sender chain even if its report is invalid.
                            // Storage keeps it out of the inbox and therefore
                            // out of prompt-gate decisions and receipt replies.
                            record_inbound_receipt(database, channel, opened, now)?;
                        }
                        if let Some(reason) = malformed_context {
                            // A valid signature authenticates that this
                            // sender authored malformed context. It does not
                            // turn a bad package into transport tampering.
                            database.reject_malformed_context(
                                &channel.channel_id,
                                &opened.envelope.message_id,
                                &reason,
                                now,
                            )?;
                        }
                    }

                    _ => {}
                }
            }
        }

        cursor = page.cursor;
        if !page.more {
            break;
        }
    }

    if let Some(roster) = roster {
        database.set_roster_progress(&channel.channel_id, roster.epoch(), roster.sequence())?;
    }
    if identity.is_some()
        && let Some(cursor) = cursor
    {
        database.set_receive_cursor(&channel.channel_id, &cursor)?;
    }

    Ok(received)
}

/// Verifies an arriving receipt and records what it reports.
///
/// The verification is the point. `accept_receipt` refuses a report naming a
/// message this installation did not send, or coming from a device that was
/// never among its recipients, and it refuses outright rather than recording
/// the claim and discounting it later — a stored claim tends to be read as a
/// fact.
///
/// A receipt that fails verification is dropped rather than raised. It is a
/// remote party's assertion about our own state, and the sender of a bad one
/// should not be able to interrupt synchronization for everyone by publishing
/// it.
pub(super) fn record_inbound_receipt(
    database: &Database,
    channel: &hrc_storage::ChannelRecord,
    opened: &hrc_core::message::QuarantinedMessage,
    now: &str,
) -> Result<()> {
    let Ok(body) =
        serde_json::from_value::<hrc_protocol::receipt::ReceiptBody>(opened.envelope.body.clone())
    else {
        return Ok(());
    };

    let sent: Vec<hrc_core::receipt::SentMessage> = database
        .sent_messages(&channel.channel_id)?
        .into_iter()
        .map(|facts| hrc_core::receipt::SentMessage {
            message_id: facts.message_id,
            recipient_device_ids: facts.recipient_device_ids,
            thread_id: facts.thread_id,
            kind: facts.kind,
        })
        .collect();

    let Ok(verified) = hrc_core::receipt::accept_receipt(opened, &body, &sent) else {
        return Ok(());
    };

    for report in verified {
        database.record_receipt(
            &channel.channel_id,
            &hrc_storage::RecordedReceipt {
                message_id: report.message_id,
                reporter_principal: report.reporter_principal,
                reporter_device: report.reporter_device,
                state: report.state.as_str().to_owned(),
                rejection_code: report.rejection_code,
                reported_at: report.reported_at,
            },
            now,
        )?;
    }

    Ok(())
}

/// `hrc sync --once`: run one synchronization pass for every configured channel.
pub fn sync_once(context: &Context) -> Result<Value> {
    let mut database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;

    // Reap before fetching. A message that lapsed while this installation
    // was offline should not appear as actionable for the length of one
    // synchronization pass (PRD requirement HRC-MSG-005).
    let expired = sweep_expired(&database, &now)?;

    let mut channels = Vec::new();
    for channel in database.channels()? {
        channels.push(sync_git_channel(context, &mut database, &channel, &now)?);
    }

    Ok(json!({
        "status": "ok",
        "syncedAt": now,
        "expired": expired,
        "channels": channels,
    }))
}

/// Marks lapsed messages and records one audit entry for each.
///
/// Expiry is the one inbox transition nobody decides: it is what happens
/// when no one says anything (PRD section 18.4). It is still recorded,
/// because "why did this never reach me" is a question the audit log should
/// be able to answer.
pub(super) fn sweep_expired(database: &Database, now: &str) -> Result<Vec<String>> {
    let expired = database.sweep_expired(now)?;

    for message_id in &expired {
        database.append_audit(
            None,
            Some(message_id),
            "message_expired",
            None,
            Some("the message lapsed before a local decision was made"),
            now,
        )?;
    }

    Ok(expired)
}

pub(super) fn sync_git_channel(
    context: &Context,
    database: &mut Database,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<Value> {
    if let Some(reason) = &channel.halted_reason {
        return Err(hrc_core::CoreError::SynchronizationHalted {
            reason: reason.clone(),
        }
        .into());
    }

    if channel.transport_kind != "git" {
        return Err(CliError::Io {
            action: "open the configured transport",
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported transport {}", channel.transport_kind),
            ),
        });
    }

    let git_dir = context.paths.channel_transport(&channel.channel_id);
    if let Some(parent) = git_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
            action: "create the local transport directory",
            source,
        })?;
    }

    let mut transport = GitTransport::open(&git_dir, &channel.transport_locator)?;
    let remote_head = transport.remote_head()?;
    let local_head = match transport.open_group() {
        Ok(group) => group.revision,
        Err(hrc_transport::TransportError::NoSuchGroup) => None,
        Err(error) => return Err(error.into()),
    };
    if remote_head.is_none() {
        let error = hrc_core::CoreError::Transport(
            "the remote channel branch disappeared after registration".into(),
        );
        return Err(hrc_core::sync::halt_synchronization(
            database,
            &channel.channel_id,
            error,
            now,
        )
        .into());
    }

    let remote_changed = remote_head != channel.sync_cursor;
    let recovered = database.recover_reservations(&channel.channel_id, now)?;

    let local_out_of_sync = local_head != remote_head;
    if remote_changed || local_out_of_sync {
        transport.sync_from_remote()?;
    }

    if remote_changed || local_out_of_sync {
        validate_trusted_cursor(&transport, database, channel, now)?;
    }

    let received = if channel.receive_cursor != remote_head {
        receive_for(context, channel, now).map_err(|error| {
            halt_if_received_history_is_invalid(database, &channel.channel_id, error, now)
        })?
    } else {
        Vec::new()
    };

    let fetch = if remote_changed {
        Some(hrc_core::sync::fetch_once(
            &transport,
            database,
            &channel.channel_id,
            100,
            now,
        )?)
    } else {
        None
    };

    let device = match context.passphrase.clone() {
        Some(passphrase) => {
            let store = PassphraseStore::open(context.paths.keys(), passphrase)?;
            if store.contains(DEVICE_KEY_NAME)? {
                Some(store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?)
            } else {
                None
            }
        }
        None => None,
    };

    let published = publish_pending_outgoing(
        &mut transport,
        database,
        &channel.channel_id,
        device.as_ref(),
        3,
        now,
    )?;

    let receipt_error = if context.passphrase.is_some() {
        match publish_delivery_receipts(context, channel, now) {
            Ok(()) => None,
            Err(error) => {
                let detail = error.to_string();
                database.append_audit(
                    Some(&channel.channel_id),
                    None,
                    "delivery_receipt_deferred",
                    None,
                    Some(&detail),
                    now,
                )?;
                Some(detail)
            }
        }
    } else {
        None
    };
    let final_cursor = transport.open_group()?.revision;
    if let Some(cursor) = final_cursor.as_deref() {
        // Locally published commits are trusted history too. Recording the
        // final local head makes a later remote rollback of our own message
        // or receipt visible even when the remote lands exactly on the
        // cursor that preceded that publication.
        database.set_sync_cursor(&channel.channel_id, cursor)?;
    }

    let published_messages = published
        .iter()
        .flat_map(|outcome| outcome.published.iter().cloned())
        .collect::<Vec<_>>();
    let deferred_messages = published
        .iter()
        .flat_map(|outcome| outcome.deferred.iter().cloned())
        .collect::<Vec<_>>();
    let conflicts = published
        .iter()
        .map(|outcome| outcome.conflicts)
        .sum::<u32>();

    Ok(json!({
        "channelId": channel.channel_id,
        "transport": channel.transport_kind,
        "locator": channel.transport_locator,
        "remoteChanged": remote_changed,
        "remoteHead": remote_head,
        "recoveredReservations": recovered,
        "publishedMessages": published_messages,
        "deferredMessages": deferred_messages,
        "publishConflicts": conflicts,
        "fetchedPublications": fetch.as_ref().map(|outcome| outcome.publications).unwrap_or(0),
        "fetchedControlObjects": fetch.as_ref().map(|outcome| outcome.control_objects).unwrap_or(0),
        "fetchedMessageObjects": fetch.as_ref().map(|outcome| outcome.message_objects).unwrap_or(0),
        "receivedMessages": received,
        "receiptError": receipt_error,
        "cursor": final_cursor.or_else(|| fetch.and_then(|outcome| outcome.cursor)),
    }))
}

pub(super) fn publish_pending_outgoing(
    transport: &mut GitTransport,
    database: &mut Database,
    channel_id: &str,
    device: Option<&DeviceSecrets>,
    max_attempts: u32,
    now: &str,
) -> Result<Vec<hrc_core::sync::PublishOutcome>> {
    let mut blocked_devices = std::collections::HashSet::new();
    let mut outcomes = Vec::new();

    for message_id in database.pending_outgoing(channel_id)? {
        let Some(outgoing) = database.pending_outgoing_record(&message_id)? else {
            continue;
        };
        if blocked_devices.contains(&outgoing.device_id) {
            outcomes.push(hrc_core::sync::PublishOutcome {
                published: Vec::new(),
                conflicts: 0,
                deferred: vec![outgoing.message_id],
            });
            continue;
        }

        let device_id = outgoing.device_id.clone();
        let outcome = publish_outgoing(
            transport,
            database,
            channel_id,
            outgoing,
            device,
            max_attempts,
            now,
        )?;
        if !outcome.deferred.is_empty() {
            blocked_devices.insert(device_id);
        }
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

pub(super) fn publish_outgoing(
    transport: &mut GitTransport,
    database: &mut Database,
    channel_id: &str,
    mut outgoing: PendingOutgoing,
    device: Option<&DeviceSecrets>,
    max_attempts: u32,
    now: &str,
) -> Result<hrc_core::sync::PublishOutcome> {
    let message_id = outgoing.message_id.clone();
    let object_name =
        outgoing_message_object(&outgoing.message_id, &outgoing.created_at, Vec::new()).name;
    let mut known_ciphertexts = vec![outgoing.ciphertext.clone()];
    Ok(hrc_core::sync::publish_one_prepared(
        transport,
        database,
        channel_id,
        &message_id,
        |transport, database| {
            let channel = database.channel(channel_id)?.ok_or_else(|| {
                hrc_core::CoreError::UnknownChannelState {
                    channel_id: channel_id.to_owned(),
                }
            })?;
            validate_trusted_cursor(transport, database, &channel, now)
                .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

            let mut object_exists = false;
            for ciphertext in &known_ciphertexts {
                let expected = canonical::sha256_hex(ciphertext);
                match transport.get_object(&object_name, &expected) {
                    Ok(_) => {
                        return Ok(hrc_core::sync::PreparedPublication::AlreadyPublished);
                    }
                    Err(hrc_transport::TransportError::NoSuchObject { .. }) => {
                        object_exists = false;
                        break;
                    }
                    Err(hrc_transport::TransportError::ObjectHashMismatch { .. }) => {
                        object_exists = true;
                    }
                    Err(error) => {
                        return Err(hrc_core::CoreError::Transport(error.to_string()));
                    }
                }
            }
            if object_exists {
                return Err(hrc_core::sync::halt_synchronization(
                    database,
                    channel_id,
                    hrc_core::CoreError::Transport(
                        hrc_transport::TransportError::ObjectHashMismatch {
                            name: object_name.clone(),
                        }
                        .to_string(),
                    ),
                    now,
                ));
            }

            let (roster, _) = load_roster(transport, channel_id).map_err(|error| {
                hrc_core::sync::halt_synchronization(
                    database,
                    channel_id,
                    hrc_core::CoreError::Transport(error.to_string()),
                    now,
                )
            })?;

            if outgoing.roster_epoch > roster.epoch() {
                return Err(hrc_core::CoreError::EpochOutOfOrder {
                    expected: roster.epoch(),
                    found: outgoing.roster_epoch,
                });
            }

            if outgoing.roster_epoch < roster.epoch() || outgoing.recipient_order_stale {
                let Some(device) = device else {
                    return Ok(hrc_core::sync::PreparedPublication::Defer);
                };
                if outgoing.reseal_material.is_none() {
                    database.record_attempt_failure(
                        &outgoing.message_id,
                        "queued message predates re-encryption support and cannot be resealed",
                        now,
                    )?;
                    return Ok(hrc_core::sync::PreparedPublication::Defer);
                }

                let material = outgoing.reseal_material.as_deref().ok_or_else(|| {
                    hrc_core::CoreError::OutboxMessageMismatch {
                        message_id: outgoing.message_id.clone(),
                        field: "re-encryption material",
                    }
                })?;
                let identity = device.device_identity()?;
                let plaintext = identity.decrypt(material)?;
                let envelope: MessageEnvelope =
                    canonical::from_json_str(std::str::from_utf8(&plaintext).map_err(|_| {
                        hrc_core::CoreError::MalformedMessage {
                            reason: "protected outbox material is not UTF-8".into(),
                        }
                    })?)?;
                let recipients = hrc_core::message::intended_recipients(&roster, &envelope)?
                    .into_iter()
                    .map(|recipient| recipient.device_id.clone())
                    .collect::<Vec<_>>();
                let mut replacement = None;
                database.reseal_outgoing(
                    &outgoing.message_id,
                    outgoing.roster_epoch,
                    roster.epoch(),
                    &recipients,
                    now,
                    |stored, predecessors| {
                        let ciphertext = hrc_core::message::reseal_outgoing_with_predecessors(
                            &roster,
                            &identity,
                            &device.signing_key(),
                            stored,
                            predecessors.clone(),
                        )?;
                        replacement = Some(ciphertext.clone());
                        Ok::<_, hrc_core::CoreError>(ciphertext)
                    },
                )?;
                let ciphertext =
                    replacement.ok_or_else(|| hrc_core::CoreError::OutboxMessageMismatch {
                        message_id: outgoing.message_id.clone(),
                        field: "replacement ciphertext",
                    })?;
                outgoing.roster_epoch = roster.epoch();
                outgoing.ciphertext = ciphertext;
                outgoing.recipient_order_stale = false;
                known_ciphertexts.push(outgoing.ciphertext.clone());
            }

            Ok(hrc_core::sync::PreparedPublication::Publish(
                outgoing_message_object(
                    &outgoing.message_id,
                    &outgoing.created_at,
                    outgoing.ciphertext.clone(),
                ),
            ))
        },
        max_attempts,
        now,
    )?)
}

pub(super) fn validate_trusted_cursor(
    transport: &GitTransport,
    database: &Database,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<()> {
    let Some(cursor) = channel.sync_cursor.as_deref() else {
        return Ok(());
    };

    transport
        .fetch(Some(cursor), 1)
        .map(|_| ())
        .map_err(|error| {
            let error = hrc_core::CoreError::Transport(error.to_string());
            hrc_core::sync::halt_synchronization(database, &channel.channel_id, error, now).into()
        })
}

pub(super) fn halt_if_received_history_is_invalid(
    database: &Database,
    channel_id: &str,
    error: CliError,
    now: &str,
) -> CliError {
    match error {
        CliError::Core(_) | CliError::Protocol(_) | CliError::Transport(_) => {
            let error = hrc_core::CoreError::Transport(error.to_string());
            hrc_core::sync::halt_synchronization(database, channel_id, error, now).into()
        }
        other => other,
    }
}

pub(super) fn outgoing_message_object(
    message_id: &str,
    created_at: &str,
    ciphertext: Vec<u8>,
) -> hrc_transport::PublishObject {
    let year = created_at.get(0..4).unwrap_or("0000");
    let month = created_at.get(5..7).unwrap_or("00");

    hrc_transport::PublishObject {
        name: format!("messages/{year}/{month}/{message_id}.age"),
        class: ObjectClass::Message,
        bytes: ciphertext,
    }
}
