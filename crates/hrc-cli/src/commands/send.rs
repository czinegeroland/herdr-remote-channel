//! Writing messages: notes, questions, replies and delegation requests.

use super::*;

/// `hrc send`, `hrc ask`, and `hrc reply` share this path.
///
/// Sealing needs the roster, because the recipient set is derived from it
/// rather than supplied: a caller that could pass a list separately could
/// commit to one set and encrypt to another (HRC-SEC-016). Allocation comes
/// before sealing, so a crash between them loses a sequence number rather
/// than a message the user believes was sent.
pub(super) fn compose(
    context: &Context,
    kind: hrc_protocol::MessageKind,
    recipient: &str,
    text: &str,
    in_reply_to: Option<&str>,
    expires: Option<&str>,
) -> Result<Value> {
    compose_body(
        context,
        kind,
        recipient,
        serde_json::json!({ "text": text }),
        in_reply_to,
        expires,
    )
}

/// Sends an arbitrary, structured message body through the ordinary durable
/// outbox and encrypted publication path.
pub(super) fn compose_body(
    context: &Context,
    kind: hrc_protocol::MessageKind,
    recipient: &str,
    body: Value,
    in_reply_to: Option<&str>,
    expires: Option<&str>,
) -> Result<Value> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let mut database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    // An endpoint is advisory: it asks the receiving human to consider
    // routing the message somewhere, and decides nothing on their machine.
    let (principal_id, endpoint) = match recipient.split_once('/') {
        Some((principal_id, endpoint)) => (principal_id, Some(endpoint.to_owned())),
        None => (recipient, None),
    };

    // A lifetime is resolved to an absolute timestamp here, for the same
    // reason an invite's is: the protocol compares timestamps, and a signed
    // envelope carrying "24h" would lapse at a moment that depends on when
    // someone read it rather than when it was sent.
    let expires_at = match expires {
        Some(lifetime) => Some(expiry_from(&now, lifetime)?),
        None => None,
    };

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let device_id = local_device_id(&roster, &device)?;
    let message_id = hrc_protocol::ulid_at(unix_milliseconds()?)?;

    let (thread_id, in_reply_to) = match in_reply_to {
        // A reply continues the thread of what it answers, so the thread is
        // read from local state rather than chosen: a sender that picked its
        // own would be able to attach a reply to any conversation.
        //
        // A message this installation sent is looked up too, for a follow-up
        // to its own message such as cancelling a task it delegated. Its
        // thread is the one recorded when it was written here.
        Some(message_id) => {
            let received = database
                .inbox_entries(&channel.channel_id)?
                .into_iter()
                .find(|entry| entry.message_id == message_id)
                .map(|entry| entry.thread_id.unwrap_or_else(|| message_id.to_owned()));
            let thread_id = match received {
                Some(thread_id) => thread_id,
                None => database
                    .sent_messages(&channel.channel_id)?
                    .into_iter()
                    .find(|sent| sent.message_id == message_id)
                    .map(|sent| sent.thread_id)
                    .ok_or_else(|| CliError::NoSuchMessage {
                        message_id: message_id.to_owned(),
                    })?,
            };

            (thread_id, Some(message_id.to_owned()))
        }
        None => (message_id.clone(), None),
    };

    let payload_hash = canonical::canonical_sha256_hex(&body)?;

    let envelope = MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: channel.channel_id.clone(),
        roster_epoch: roster.epoch(),
        message_id: message_id.clone(),
        device_sequence: 0,
        previous_chain_id: None,
        created_at: now.clone(),
        expires_at: expires_at.clone(),
        to: hrc_protocol::Addressing {
            principals: vec![principal_id.to_owned()],
            endpoint,
        },
        recipients: hrc_protocol::RecipientDevices::new([device_id.clone()])?,
        recipient_previous_chain_ids: None,
        thread_id: thread_id.clone(),
        in_reply_to,
        kind: kind.as_str().to_owned(),
        requested_capability: None,
        body,
        attachments: Vec::new(),
        padding: String::new(),
    };

    let addressed: Vec<String> = hrc_core::message::intended_recipients(&roster, &envelope)?
        .into_iter()
        .map(|device| device.device_id.clone())
        .collect();
    let local_recipient = device.device_identity()?.recipient();
    database.compose_outgoing(
        &channel.channel_id,
        &device_id,
        &message_id,
        roster.epoch(),
        &payload_hash,
        &thread_id,
        kind.as_str(),
        envelope.in_reply_to.as_deref(),
        &addressed,
        &now,
        |reservation| {
            let mut envelope = envelope.clone();
            envelope.device_sequence = reservation.device_sequence;
            envelope.previous_chain_id = reservation.previous_chain_id.clone();
            envelope.recipient_previous_chain_ids =
                Some(reservation.recipient_previous_chain_ids.clone());
            let reseal_plaintext = canonical::to_canonical_bytes(&envelope)?;
            let reseal_material =
                hrc_crypto::encrypt_to(std::slice::from_ref(&local_recipient), &reseal_plaintext)?;
            let ciphertext = hrc_core::message::seal_with_predecessors(
                &roster,
                &device.signing_key(),
                Signer {
                    principal_id: principal.signing_key().verifying_key().to_base64url(),
                    device_id: device_id.clone(),
                },
                envelope,
                reservation.recipient_previous_chain_ids.clone(),
            )?;
            Ok::<_, CliError>(hrc_storage::OutgoingBuild {
                ciphertext,
                reseal_material,
            })
        },
    )?;

    let outcomes = publish_pending_outgoing(
        &mut transport,
        &mut database,
        &channel.channel_id,
        Some(&device),
        5,
        &now,
    )?;
    let published = outcomes
        .iter()
        .any(|outcome| outcome.published.contains(&message_id));

    database.append_audit(
        Some(&channel.channel_id),
        Some(&message_id),
        "send",
        Some(&payload_hash),
        Some(kind.as_str()),
        &now,
    )?;

    Ok(json!({
        "status": "ok",
        "messageId": message_id,
        "threadId": thread_id,
        "kind": kind.as_str(),
        "published": published,
        "deferred": !published,
    }))
}

/// `hrc send`: an informational note.
pub fn send(
    context: &Context,
    recipient: &str,
    text: &str,
    expires: Option<&str>,
) -> Result<Value> {
    compose(
        context,
        hrc_protocol::MessageKind::Note,
        recipient,
        text,
        None,
        expires,
    )
}

/// `hrc ask`: a question, optionally addressed to a logical endpoint.
pub fn ask(context: &Context, recipient: &str, text: &str, expires: Option<&str>) -> Result<Value> {
    compose(
        context,
        hrc_protocol::MessageKind::Question,
        recipient,
        text,
        None,
        expires,
    )
}

/// `hrc reply`: an answer in the thread of an existing message.
pub fn reply(context: &Context, message_id: &str, text: &str) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let entry = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id)
        .ok_or_else(|| CliError::NoSuchMessage {
            message_id: message_id.to_owned(),
        })?;

    compose(
        context,
        hrc_protocol::MessageKind::Answer,
        &entry.sender_principal,
        text,
        Some(message_id),
        // A reply inherits no expiry from the question it answers: expiry is
        // the sender's statement about their own message.
        None,
    )
}

/// `hrc delegate`: ask someone to do something, without being able to make
/// them.
///
/// The `task` kind is communication, never execution. `TaskBody` is proven to
/// expose no command, script, argument or environment field, and nothing on
/// the receiving side runs anything: a delegation arrives as a quarantined
/// message like any other and waits for a human.
///
/// `--context` names a package rather than carrying one. Disclosure is `hrc
/// context send`, which is a trusted operation with its own authorization, so
/// pointing at a package a recipient may already hold stays on the agent-safe
/// path where drafting a request belongs.
pub fn delegate(
    context: &Context,
    recipient: &str,
    title: &str,
    description: &str,
    criteria: &[String],
    context_id: Option<&str>,
    due: Option<&str>,
) -> Result<Value> {
    let due_by = match due {
        Some(lifetime) => {
            let database = Database::open(context.paths.database())?;
            let now = database.utc_now()?;
            Some(expiry_from(&now, lifetime)?)
        }
        None => None,
    };

    let body = hrc_protocol::delegation::TaskBody {
        title: title.to_owned(),
        description: description.to_owned(),
        acceptance_criteria: criteria.to_vec(),
        context_id: context_id.map(str::to_owned),
        due_by,
    };

    // Validated here rather than left to the receiver. A task with an empty
    // title or description is one nobody can act on, and finding that out
    // after it is published to an append-only history helps no one.
    body.validate()?;

    let mut sent = compose_body(
        context,
        hrc_protocol::MessageKind::Task,
        recipient,
        serde_json::to_value(&body).map_err(|_| {
            CliError::Core(hrc_core::CoreError::MalformedMessage {
                reason: "the delegation body could not be serialized".into(),
            })
        })?,
        None,
        // The task's own `dueBy` is when the requester stops waiting, which
        // is not the same as when the message should stop existing. Expiring
        // the message would delete the request from the recipient's inbox.
        None,
    )?;

    sent["title"] = Value::String(title.to_owned());
    Ok(sent)
}

/// `hrc result`: report the outcome of a task someone delegated to you.
///
/// The result goes to whoever asked, in the task's thread, and waits for
/// their review like any other message: `completed` is the requester's
/// verdict, never the assignee's (section 18.4), so the most this can report
/// is that a result is ready or that the work failed.
///
/// `--commit` makes part of the claim checkable. Each revision is resolved
/// here, in the sender's own checkout, and travels as a full object name; the
/// requester's review then says whether their checkout holds it. Nothing on
/// either side fetches, checks out or applies anything.
pub fn task_result(
    context: &Context,
    task_id: &str,
    summary: &str,
    failed: bool,
    commits: &[String],
    context_id: Option<&str>,
) -> Result<Value> {
    let task = delegated_task(context, task_id)?;

    // Resolved in the checkout the command runs in, which is the one the
    // person reporting the result is working in.
    let root = std::env::current_dir().map_err(|source| CliError::Io {
        action: "find the checkout a result's commits are named in",
        source,
    })?;
    let references = commits
        .iter()
        .map(|revision| super::references::resolve_commit(&root, revision))
        .map(|resolved| resolved.map(hrc_protocol::Reference::commit))
        .collect::<Result<Vec<_>>>()?;

    let body = hrc_protocol::ResultBody {
        task_id: task_id.to_owned(),
        state: if failed {
            hrc_protocol::DelegationState::Failed
        } else {
            hrc_protocol::DelegationState::ResultPendingReview
        },
        summary: summary.to_owned(),
        context_id: context_id.map(str::to_owned),
        references,
    };

    // Refused here rather than left to the receiver, as for a task.
    body.validate()?;

    let mut sent = compose_body(
        context,
        hrc_protocol::MessageKind::Result,
        &task.sender_principal,
        serde_json::to_value(&body).map_err(|_| {
            CliError::Core(hrc_core::CoreError::MalformedMessage {
                reason: "the result body could not be serialized".into(),
            })
        })?,
        Some(task_id),
        None,
    )?;

    sent["references"] = json!(
        body.references
            .iter()
            .map(|reference| reference.id.clone())
            .collect::<Vec<_>>()
    );
    Ok(sent)
}

/// `hrc task accept|decline|progress`: report where a task someone gave
/// you stands.
///
/// Sent to whoever asked, in the task's thread, like a result. The body is
/// `{taskId, state, note}` and must agree with the kind it travels as, so a
/// `task_accept` cannot say `declined`. Concluding a task is `hrc result`'s
/// job, and judging it complete is the requester's.
pub fn task_report(
    context: &Context,
    task_id: &str,
    kind: hrc_protocol::MessageKind,
    state: hrc_protocol::DelegationState,
    note: Option<&str>,
) -> Result<Value> {
    let task = delegated_task(context, task_id)?;

    let body = hrc_protocol::ProgressBody {
        task_id: task_id.to_owned(),
        state,
        note: note.unwrap_or_default().to_owned(),
    };
    body.validate_as(kind)?;

    let mut sent = compose_body(
        context,
        kind,
        &task.sender_principal,
        serde_json::to_value(&body).map_err(|_| {
            CliError::Core(hrc_core::CoreError::MalformedMessage {
                reason: "the task report could not be serialized".into(),
            })
        })?,
        Some(task_id),
        None,
    )?;

    sent["state"] = json!(state.as_str());
    Ok(sent)
}

/// `hrc task cancel`: withdraw a task this installation delegated.
///
/// Best effort, as section 18.2 says of `cancel`: it tells the assignee the
/// request is withdrawn, and cannot stop anything they already did. Sent in
/// the task's thread to the principal the task was addressed to, which is
/// resolved from the devices the task was sealed to through the signed
/// roster rather than remembered as a name, so it cannot drift from who
/// actually received it.
pub fn task_cancel(context: &Context, task_id: &str, note: Option<&str>) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let sent = database
        .sent_messages(&channel.channel_id)?
        .into_iter()
        .find(|sent| sent.message_id == task_id)
        .ok_or_else(|| CliError::NoSuchMessage {
            message_id: task_id.to_owned(),
        })?;

    if sent.kind != hrc_protocol::MessageKind::Task.as_str() {
        return Err(CliError::NotATask {
            message_id: task_id.to_owned(),
            kind: sent.kind,
        });
    }

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let mut principals: Vec<String> = sent
        .recipient_device_ids
        .iter()
        .filter_map(|device| {
            roster
                .device(device)
                .map(|device| device.principal_id.clone())
        })
        .collect();
    principals.sort();
    principals.dedup();

    // A task is addressed to one principal. None left means every device it
    // was sealed to has since left the roster, and there is nobody to tell.
    let [assignee] = principals.as_slice() else {
        return Err(CliError::TaskRecipientGone {
            message_id: task_id.to_owned(),
        });
    };

    let body = hrc_protocol::ProgressBody {
        task_id: task_id.to_owned(),
        state: hrc_protocol::DelegationState::Cancelled,
        note: note.unwrap_or_default().to_owned(),
    };
    body.validate_as(hrc_protocol::MessageKind::Cancel)?;

    let mut sent = compose_body(
        context,
        hrc_protocol::MessageKind::Cancel,
        assignee,
        serde_json::to_value(&body).map_err(|_| {
            CliError::Core(hrc_core::CoreError::MalformedMessage {
                reason: "the cancellation could not be serialized".into(),
            })
        })?,
        Some(task_id),
        None,
    )?;

    sent["state"] = json!(body.state.as_str());
    Ok(sent)
}

/// The task a report or result is about: a `task` this installation
/// received, by the message identifier it arrived as.
fn delegated_task(context: &Context, task_id: &str) -> Result<hrc_storage::InboxEntry> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let task = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == task_id)
        .ok_or_else(|| CliError::NoSuchMessage {
            message_id: task_id.to_owned(),
        })?;

    if task.kind != hrc_protocol::MessageKind::Task.as_str() {
        return Err(CliError::NotATask {
            message_id: task_id.to_owned(),
            kind: task.kind,
        });
    }

    Ok(task)
}

/// Milliseconds since the Unix epoch, for a message identifier.
///
/// Read from the system clock rather than derived from the RFC 3339
/// timestamp the rest of the record uses, because that one has second
/// precision. A ULID built from it would give every message sent within the
/// same second an identical time prefix, and their order would then be
/// decided by the random half — which is to say, not ordered at all.
///
/// Sub-second ties remain possible and remain unordered; that is what a ULID
/// promises and no more. Per-device order does not depend on this: the chain
/// in section 18.1 establishes it, and the inbox reads by arrival.
pub(super) fn unix_milliseconds() -> Result<u64> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CliError::Entropy)?;

    Ok(since_epoch.as_millis() as u64)
}

/// Turns a lifetime such as `24h` into an absolute RFC 3339 expiry.
///
/// The protocol compares timestamps, not durations: an invite carrying "24h"
/// would be compared lexicographically against a date and lapse at some
/// arbitrary moment that depends on the year. Resolving it here means the
/// value the channel sees is one every participant can evaluate.
pub(super) fn expiry_from(now: &str, lifetime: &str) -> Result<String> {
    let seconds = parse_lifetime(lifetime).ok_or_else(|| CliError::InvalidLifetime {
        value: lifetime.to_owned(),
    })?;

    let start = hrc_core::time::epoch_seconds(now).ok_or_else(|| CliError::InvalidLifetime {
        value: now.to_owned(),
    })?;

    Ok(hrc_core::time::rfc3339_from(start + seconds))
}

/// Parses `30m`, `24h`, or `7d` into seconds.
pub(super) fn parse_lifetime(value: &str) -> Option<i64> {
    let value = value.trim();
    let (digits, unit) = value.split_at(value.len().checked_sub(1)?);
    let amount: i64 = digits.parse().ok()?;

    if amount <= 0 {
        return None;
    }

    match unit {
        "m" => Some(amount * 60),
        "h" => Some(amount * 3600),
        "d" => Some(amount * 86_400),
        _ => None,
    }
}
