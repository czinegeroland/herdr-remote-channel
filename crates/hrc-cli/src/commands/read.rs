//! Reading what arrived: the inbox, a message, a thread, and waiting.

use super::*;

/// `hrc inbox`: the closed metadata set, never a pending body.
pub fn inbox(context: &Context, pending_only: bool) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let entries: Vec<Value> = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .filter(|entry| !pending_only || entry.disposition == "quarantined")
        .map(|entry| {
            json!({
                "messageId": entry.message_id,
                "sender": entry.sender_principal,
                "kind": entry.kind,
                "threadId": entry.thread_id,
                "inReplyTo": entry.in_reply_to,
                "arrival": entry.arrival_sequence,
                "expiresAt": entry.expires_at,
                "disposition": entry.disposition,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "channelId": channel.channel_id,
        "entries": entries,
    }))
}

/// `hrc show`: everything known about one message, without disclosing a
/// body nobody approved.
///
/// The rule is the same one the inbox and the plugin pane follow, and it is
/// why this is not simply "print the message". An inbound body stays sealed
/// until a human releases it through the trusted interface, so what this
/// shows for a quarantined message is the agent-safe metadata and the reason
/// the body is absent — never the body itself.
///
/// A message this installation sent is different in kind: the content was
/// never quarantined, because it did not arrive from anyone. What matters
/// there is where it got to, which is the outbox state and the receipts
/// other devices published about it.
pub fn show(context: &Context, message_id: &str) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let inbound = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id);

    if let Some(entry) = inbound {
        // `released_content` returns something only for a decision that
        // actually delivers. Keeping a message in the inbox and declining it
        // are decisions too, and neither authorizes disclosure.
        let released = released_content(&database, message_id)?;

        return Ok(json!({
            "status": "ok",
            "messageId": entry.message_id,
            "direction": "inbound",
            "sender": entry.sender_principal,
            "kind": entry.kind,
            "threadId": entry.thread_id,
            "inReplyTo": entry.in_reply_to,
            "arrival": entry.arrival_sequence,
            "expiresAt": entry.expires_at,
            "disposition": entry.disposition,
            "body": match released {
                Some(text) => Value::String(text),
                None => Value::Null,
            },
            "bodyWithheld": match entry.disposition.as_str() {
                "quarantined" => Some("awaiting a decision in the trusted interface"),
                "declined" => Some("declined"),
                "expired" => Some("expired before a decision"),
                _ => None,
            },
        }));
    }

    let Some(state) = database.outbox_state(message_id)? else {
        return Err(CliError::NoSuchMessage {
            message_id: message_id.to_owned(),
        });
    };

    let receipts: Vec<Value> = database
        .receipts_for(message_id)?
        .into_iter()
        .map(|receipt| {
            json!({
                "reporter": receipt.reporter_principal,
                "device": receipt.reporter_device,
                "state": receipt.state,
                "rejectionCode": receipt.rejection_code,
                "reportedAt": receipt.reported_at,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "messageId": message_id,
        "direction": "outbound",
        "state": state.as_str(),
        "receipts": receipts,
    }))
}

/// `hrc wait`: block until a message reaches a state, or give up.
///
/// The states are the ones the rest of the system already records, not a new
/// vocabulary: an outbound message reaches `published` in the outbox and then
/// `delivered`, `read`, `accepted` or `rejected` when a recipient device says
/// so, and an inbound message reaches a disposition when a human decides.
///
/// Each pass runs a real synchronization rather than only reading local
/// state. Without one, waiting on a machine whose daemon is not running would
/// block until the timeout no matter what the channel did — and section 17.1
/// names explicit `hrc wait` as the reason a five-second poll exists.
pub fn wait(
    context: &Context,
    message_id: &str,
    until: Option<&str>,
    timeout: Option<&str>,
) -> Result<Value> {
    const DEFAULT_STATE: &str = "delivered";
    const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

    let wanted = until.unwrap_or(DEFAULT_STATE);
    let limit = match timeout {
        Some(value) => Duration::from_secs(parse_wait_timeout(value)?),
        None => Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
    };

    // Section 17.1's explicit-wait interval, and the adapter's own minimum
    // where that is longer: a provider's rate limit is a hard constraint and
    // exceeding it throttles every channel the installation hosts.
    let interval = {
        let database = Database::open(context.paths.database())?;
        let channel = only_channel(&database)?;
        let transport = GitTransport::open(
            context.paths.channel_transport(&channel.channel_id),
            &channel.transport_locator,
        )?;
        let minimum =
            Duration::from_secs(transport.capabilities().min_poll_interval_seconds as u64);
        poll_interval(PollActivity::Waiting, minimum)
    };

    let started = std::time::Instant::now();

    loop {
        if let Some(state) = reached_state(context, message_id, wanted)? {
            return Ok(json!({
                "status": "ok",
                "messageId": message_id,
                "state": state,
                "waitedSeconds": started.elapsed().as_secs(),
            }));
        }

        if started.elapsed() >= limit {
            // A timeout is not an error. The caller asked how things stand
            // after a bounded wait, and "not yet" is an answer to that.
            return Ok(json!({
                "status": "ok",
                "messageId": message_id,
                "state": "timeout",
                "waitedSeconds": started.elapsed().as_secs(),
            }));
        }

        std::thread::sleep(interval.min(limit.saturating_sub(started.elapsed())));
        if started.elapsed() >= limit {
            continue;
        }

        // Failing to reach the transport is not a reason to stop waiting: the
        // state may still arrive through a daemon running alongside this, and
        // a caller that asked for a timeout asked to be told at the end of it.
        if let Err(error) = sync_once(context) {
            let database = Database::open(context.paths.database())?;
            database.append_audit(
                None,
                Some(message_id),
                "wait_sync_failed",
                None,
                Some(&error.to_string()),
                &database.utc_now()?,
            )?;
        }
    }
}

/// The state a message is in now, in the vocabulary `hrc wait` accepts.
///
/// Receipts win over the outbox for an outbound message, because `published`
/// is what this installation did and a receipt is what happened to it. The
/// most advanced report is the one returned: a device that has read a message
/// also received it, and reporting `delivered` after `read` would go
/// backwards.
pub(super) fn reached_state(
    context: &Context,
    message_id: &str,
    wanted: &str,
) -> Result<Option<String>> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;
    let mut observed = Vec::new();

    if let Some(entry) = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id)
    {
        observed.push(entry.disposition);
    }

    let reported = database.receipts_for(message_id)?;
    for state in ["rejected", "accepted", "read", "delivered"] {
        if reported.iter().any(|receipt| receipt.state == state) {
            observed.push(state.to_owned());
        }
    }

    if let Some(state) = database.outbox_state(message_id)? {
        observed.push(state.as_str().to_owned());
    }
    Ok(observed
        .into_iter()
        .filter(|state| state_reaches(state, wanted))
        .max_by_key(|state| state_rank(state)))
}

pub(super) fn state_rank(state: &str) -> u8 {
    match state {
        "accepted" | "approved" | "edited" => 9,
        "read" => 8,
        "delivered" => 7,
        "rejected" | "declined" | "expired" => 6,
        "published" => 5,
        "publishing" => 4,
        "queued" => 3,
        "reserved" => 2,
        "quarantined" => 1,
        _ => 0,
    }
}

pub(super) fn state_reaches(observed: &str, wanted: &str) -> bool {
    if observed == wanted {
        return true;
    }
    match wanted {
        "reserved" => matches!(
            observed,
            "queued" | "publishing" | "published" | "delivered" | "read" | "accepted" | "rejected"
        ),
        "queued" => matches!(
            observed,
            "publishing" | "published" | "delivered" | "read" | "accepted" | "rejected"
        ),
        "publishing" | "published" => {
            matches!(
                observed,
                "published" | "delivered" | "read" | "accepted" | "rejected"
            )
        }
        "delivered" => matches!(observed, "read" | "accepted"),
        "read" => observed == "accepted",
        "quarantined" => matches!(observed, "approved" | "edited" | "declined" | "expired"),
        _ => false,
    }
}

/// Parses `30s`, `5m`, or `2h` into seconds.
///
/// Separate from `parse_lifetime`, which governs invite and message expiry on
/// the wire. That one deliberately has no seconds unit, because a lifetime
/// measured in seconds is not a useful thing to publish; a wait measured in
/// seconds is entirely reasonable.
pub(super) fn parse_wait_timeout(value: &str) -> Result<u64> {
    let invalid = || CliError::InvalidLifetime {
        value: value.to_owned(),
    };

    let value = value.trim();
    let (digits, unit) = value
        .split_at_checked(value.len().checked_sub(1).ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    let amount: u64 = digits.parse().map_err(|_| invalid())?;

    if amount == 0 {
        return Err(invalid());
    }

    match unit {
        "s" => Ok(amount),
        "m" => amount.checked_mul(60).ok_or_else(invalid),
        "h" => amount.checked_mul(3_600).ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// `hrc thread`: one conversation in local arrival order.
///
/// Pending bodies stay redacted. Ordering is by arrival rather than by the
/// sender's timestamp, which a sender chooses.
pub fn thread(context: &Context, thread_id: &str) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let entries: Vec<Value> = database
        .thread_entries(&channel.channel_id, thread_id)?
        .into_iter()
        .map(|entry| {
            json!({
                "messageId": entry.message_id,
                "sender": entry.sender_principal,
                "kind": entry.kind,
                "arrival": entry.arrival_sequence,
                "disposition": entry.disposition,
                "body": if entry.disposition == "quarantined" {
                    Value::String("[redacted until approved]".into())
                } else {
                    Value::Null
                },
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "threadId": thread_id,
        "entries": entries,
    }))
}

/// The content a human released for one message, if they released any.
///
/// Only a decision that actually delivers produces readable content.
/// Keeping a message in the inbox and declining it are decisions too, and
/// neither authorizes disclosure — so this returns `None` for both, and the
/// agent-safe surface cannot tell them apart from a message that was never
/// decided at all.
pub(super) fn released_content(database: &Database, message_id: &str) -> Result<Option<String>> {
    let Some(decision) = database
        .decisions_for(message_id)?
        .into_iter()
        .rfind(|decision| {
            matches!(
                decision.action.as_str(),
                "deliver_to_agent" | "deliver_edited"
            )
        })
    else {
        return Ok(None);
    };

    // The edit is what the human approved when they made one. Returning the
    // original in that case would hand an agent text a human chose not to
    // send it.
    let body = decision
        .edited_content
        .clone()
        .unwrap_or_else(|| decision.original_content.clone());

    let channel_local_name = database
        .channel(&decision.channel_id)?
        .map(|channel| channel.local_name)
        .unwrap_or_else(|| decision.channel_id.clone());

    let sender_principal = database
        .inbox_entries(&decision.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id)
        .map(|entry| entry.sender_principal)
        .unwrap_or_default();

    // Reframed rather than stored framed: the banner names the channel and
    // the approver as they are known now, and a banner kept as text would be
    // one more place a sender-influenced string could be edited into.
    let banner = hrc_core::gate::provenance_banner(
        &sender_principal,
        &channel_local_name,
        message_id,
        &decision.decided_by,
    );

    Ok(Some(format!("{banner}{body}")))
}
