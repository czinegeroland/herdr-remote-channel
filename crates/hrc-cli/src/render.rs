//! Turning a command's result into output.
//!
//! Both output modes render the same value, so they cannot drift: `--json`
//! is a formatting choice, not a second code path that might report
//! something different from what a person sees.
//!
//! PRD section 27 requires that machine-readable output stay separate from
//! human formatting, and that neither rely on color alone. Nothing here
//! emits color.

use serde_json::Value;

/// Prints a successful result.
pub fn success(as_json: bool, command: &str, value: &Value) {
    if as_json {
        println!("{value}");
        return;
    }

    match command {
        "init" => render_init(value),
        "whoami" => render_whoami(value),
        "channels" => render_channels(value),
        "status" => render_status(value),
        "doctor" => render_doctor(value),
        "sync" => render_sync(value),
        "daemon" => render_daemon(value),
        "audit" => render_audit(value),
        "invite create" | "invite list" | "invite revoke" => render_invite(value),
        "join" | "join pending" => render_join(value),
        "send" | "ask" | "reply" => render_sent(value),
        "inbox" => render_inbox(value),
        "members" => render_members(value),
        "device list" => render_devices(value),
        "thread" => render_thread(value),
        "herdr startup" => render_herdr_startup(value),
        "herdr manifest" => render_herdr_manifest(value),
        "herdr action" | "herdr pane" => render_herdr_pane(value),
        _ => println!("{value}"),
    }
}

/// Prints the manifest file itself, so the command can be redirected.
///
/// `hrc herdr manifest > herdr-plugin.toml` has to produce a file Herdr can
/// read, which means no framing, no trailing summary, and no `println!` of a
/// JSON blob. `--json` is the structured form for anything that wants one.
fn render_herdr_manifest(value: &Value) {
    print!("{}", text(value, "toml"));
}

fn render_init(value: &Value) {
    println!("Initialized {}", text(value, "home"));
    println!("  signing key         {}", text(value, "signingKey"));
    println!(
        "  encryption recipient {}",
        text(value, "encryptionRecipient")
    );
}

fn render_whoami(value: &Value) {
    println!("signing key          {}", text(value, "signingKey"));
    println!(
        "encryption recipient {}",
        text(value, "encryptionRecipient")
    );
}

fn render_channels(value: &Value) {
    let channels = array(value, "channels");
    if channels.is_empty() {
        println!("No channels. Create one with `hrc create --repo <owner/name>`.");
        return;
    }

    for channel in channels {
        let halted = if channel["halted"].as_bool().unwrap_or(false) {
            "  [HALTED]"
        } else {
            ""
        };
        println!(
            "{}  {} ({}){halted}",
            text(channel, "localName"),
            text(channel, "locator"),
            text(channel, "transport"),
        );
    }
}

fn render_status(value: &Value) {
    println!("state directory {}", text(value, "home"));

    let channels = array(value, "channels");
    if channels.is_empty() {
        println!("no channels configured");
        return;
    }

    for channel in channels {
        println!();
        println!("{}", text(channel, "localName"));
        println!("  channel        {}", text(channel, "channelId"));
        println!(
            "  transport      {} {}",
            text(channel, "transport"),
            text(channel, "locator")
        );
        println!("  roster epoch   {}", number(channel, "rosterEpoch"));
        println!(
            "  last fetched   {}",
            optional(channel, "lastFetchedRevision", "never")
        );
        println!("  outbox pending {}", number(channel, "outboxPending"));
        println!("  unread         {}", number(channel, "unread"));
        println!("  approvals      {}", number(channel, "pendingApproval"));

        if let Some(error) = channel["lastTransportError"].as_str() {
            println!("  last error     {error}");
        }
        if let Some(reason) = channel["haltedReason"].as_str() {
            // Stated without relying on color, per PRD section 27.
            println!("  HALTED         {reason}");
            println!("                 synchronization will not resume on its own");
        }
    }
}

fn render_doctor(value: &Value) {
    for check in array(value, "checks") {
        // "ok" and "FAILED" differ in text, not only in color.
        let mark = if check["ok"].as_bool().unwrap_or(false) {
            "ok    "
        } else {
            "FAILED"
        };
        println!(
            "{mark}  {:<18}  {}",
            text(check, "check"),
            text(check, "detail")
        );
    }

    println!();
    if value["healthy"].as_bool().unwrap_or(false) {
        println!("All checks passed.");
    } else {
        println!("Some checks failed. See the lines marked FAILED above.");
    }
}

fn render_audit(value: &Value) {
    let entries = array(value, "entries");
    if entries.is_empty() {
        println!("No audit entries.");
        return;
    }

    for entry in entries {
        println!("{}  {}", text(entry, "occurredAt"), text(entry, "action"),);
        if let Some(channel) = entry["channelId"].as_str() {
            println!("  channel {}", channel);
        }
        if let Some(message) = entry["messageId"].as_str() {
            println!("  message {}", message);
        }
        if let Some(detail) = entry["detail"].as_str() {
            println!("  detail  {}", detail);
        }
    }
}

fn render_sync(value: &Value) {
    println!("synchronized at {}", text(value, "syncedAt"));

    let channels = array(value, "channels");
    if channels.is_empty() {
        println!("no channels configured");
        return;
    }

    for channel in channels {
        println!();
        println!("{}", text(channel, "channelId"));
        println!(
            "  transport       {} {}",
            text(channel, "transport"),
            text(channel, "locator")
        );
        println!(
            "  remote changed  {}",
            if channel["remoteChanged"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            }
        );
        println!(
            "  fetched         {} publications, {} control, {} messages",
            number(channel, "fetchedPublications"),
            number(channel, "fetchedControlObjects"),
            number(channel, "fetchedMessageObjects")
        );
        println!(
            "  published       {}",
            array(channel, "publishedMessages").len()
        );
        println!(
            "  deferred        {}",
            array(channel, "deferredMessages").len()
        );
        println!(
            "  recovered       {} reserved slots",
            number(channel, "recoveredReservations")
        );
    }
}

fn render_daemon(value: &Value) {
    println!("daemon synchronized at {}", text(value, "syncedAt"));
    println!("next poll in {} seconds", number(value, "nextPollSeconds"));

    let channels = array(value, "channels");
    if channels.is_empty() {
        println!("no channels configured");
        return;
    }

    for channel in channels {
        println!();
        println!("{}", text(channel, "channelId"));
        if channel["status"] == "error" {
            println!("  ERROR           {}", text(channel, "message"));
            continue;
        }

        println!(
            "  transport       {} {}",
            text(channel, "transport"),
            text(channel, "locator")
        );
        println!(
            "  remote changed  {}",
            if channel["remoteChanged"].as_bool().unwrap_or(false) {
                "yes"
            } else {
                "no"
            }
        );
        println!(
            "  fetched         {} publications, {} control, {} messages",
            number(channel, "fetchedPublications"),
            number(channel, "fetchedControlObjects"),
            number(channel, "fetchedMessageObjects")
        );
        println!(
            "  published       {}",
            array(channel, "publishedMessages").len()
        );
    }
}

/// Renders what the plugin reports at startup.
fn render_herdr_startup(value: &Value) {
    println!("{}", text(value, "sidebar"));
}

/// Renders the plugin inbox pane (PRD section 23.2).
///
/// One line per message, then its detail. Every field here is locally
/// resolved or a validated identifier; there is no body to print, which is
/// the point.
fn render_herdr_pane(value: &Value) {
    for notification in array(value, "notifications") {
        // Urgency is a word, not a color (PRD section 27).
        let mark = if notification["urgent"].as_bool().unwrap_or(false) {
            "! "
        } else {
            "  "
        };
        println!("{mark}{}", text(notification, "text"));
    }

    let rows = array(value, "rows");
    if rows.is_empty() {
        println!("Inbox is empty.");
        return;
    }

    println!();
    for row in rows {
        println!(
            "{}  {} from {}",
            text(row, "arrival_at"),
            text(row, "kind"),
            text(row, "sender_local_name")
        );
        println!(
            "  thread {}  endpoint {}",
            text(row, "thread_label"),
            if text(row, "endpoint_label").is_empty() {
                "none"
            } else {
                text(row, "endpoint_label")
            }
        );
        println!(
            "  {}  attachments {} ({} bytes)",
            text(row, "verification"),
            number(row, "attachment_count"),
            number(row, "attachment_bytes")
        );
        if let Some(expires_at) = row["expires_at"].as_str() {
            println!("  expires {expires_at}");
        }

        let decisions = array(row, "decisions");
        if decisions.is_empty() {
            println!("  no decision available");
        } else {
            println!("  awaiting your decision in `hrc review`");
        }
    }

    println!();
    println!("{} awaiting a decision", number(value, "pending"));
}

/// A string field, or an empty string.
fn text<'a>(value: &'a Value, key: &str) -> &'a str {
    value[key].as_str().unwrap_or_default()
}

/// A string field, or `fallback` when absent or null.
fn optional<'a>(value: &'a Value, key: &str, fallback: &'a str) -> &'a str {
    value[key].as_str().unwrap_or(fallback)
}

/// A numeric field, or zero.
fn number(value: &Value, key: &str) -> u64 {
    value[key].as_u64().unwrap_or_default()
}

/// An array field, or an empty slice.
fn array<'a>(value: &'a Value, key: &str) -> &'a [Value] {
    value[key].as_array().map(Vec::as_slice).unwrap_or_default()
}

/// Renders invite output.
///
/// The code is printed on its own line and labelled as a secret, because the
/// next thing that happens to it is a human copying it somewhere.
fn render_invite(value: &Value) {
    if let Some(invites) = value["invites"].as_array() {
        if invites.is_empty() {
            println!("No invites.");
            return;
        }

        for invite in invites {
            println!(
                "{}  {}  expires {}  for {}",
                invite["state"].as_str().unwrap_or("?"),
                invite["inviteId"].as_str().unwrap_or("?"),
                invite["expiresAt"].as_str().unwrap_or("?"),
                invite["intendedFor"].as_str().unwrap_or("?")
            );
        }
        return;
    }

    if let Some(code) = value["inviteCode"].as_str() {
        println!(
            "Invite for {} expires {}",
            value["intendedFor"].as_str().unwrap_or("?"),
            value["expiresAt"].as_str().unwrap_or("?")
        );
        println!();
        println!("{code}");
        println!();
        println!("Give this to them through a channel you trust. It works once.");
        return;
    }

    println!("{}", value["inviteId"].as_str().unwrap_or("done"));
}

/// Renders join output.
fn render_join(value: &Value) {
    if let Some(pending) = value["pending"].as_array() {
        if pending.is_empty() {
            println!("No join requests.");
            return;
        }

        for request in pending {
            println!(
                "{}  from {}",
                request["requestId"].as_str().unwrap_or("?"),
                request["principalId"].as_str().unwrap_or("?")
            );
            println!(
                "  safety phrase: {}",
                request["safetyPhrase"].as_str().unwrap_or("?")
            );
        }
        return;
    }

    println!(
        "Join requested on channel {}",
        value["channelId"].as_str().unwrap_or("?")
    );
    println!();
    println!(
        "Safety phrase: {}",
        value["safetyPhrase"].as_str().unwrap_or("?")
    );
    println!("Compare it with the administrator out loud before they approve.");
}

/// Renders a sent message.
fn render_sent(value: &Value) {
    println!(
        "{} {} in thread {}",
        value["kind"].as_str().unwrap_or("message"),
        value["messageId"].as_str().unwrap_or("?"),
        value["threadId"].as_str().unwrap_or("?")
    );

    if value["published"].as_bool() == Some(false) {
        println!("Queued locally; it will publish on the next synchronization.");
    }
}

/// Renders the inbox.
fn render_inbox(value: &Value) {
    let entries = value["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if entries.is_empty() {
        println!("Inbox is empty.");
        return;
    }

    for entry in entries {
        println!(
            "{:>4}  {}  {} from {}  [{}]",
            entry["arrival"].as_u64().unwrap_or(0),
            entry["messageId"].as_str().unwrap_or("?"),
            entry["kind"].as_str().unwrap_or("?"),
            entry["sender"].as_str().unwrap_or("?"),
            entry["disposition"].as_str().unwrap_or("?")
        );

        // An expiry a person can still act on is worth seeing before it
        // lapses; one that already has explains why no decision is offered.
        if let Some(expires_at) = entry["expiresAt"].as_str() {
            if entry["disposition"] == "expired" {
                println!("      expired {expires_at}; no action is available");
            } else {
                println!("      expires {expires_at}");
            }
        }
    }
}

/// Renders one thread.
fn render_thread(value: &Value) {
    let entries = value["entries"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if entries.is_empty() {
        println!("No messages in that thread.");
        return;
    }

    for entry in entries {
        println!(
            "{:>4}  {} from {}",
            entry["arrival"].as_u64().unwrap_or(0),
            entry["kind"].as_str().unwrap_or("?"),
            entry["sender"].as_str().unwrap_or("?")
        );

        if let Some(body) = entry["body"].as_str() {
            println!("      {body}");
        }
    }
}

/// Renders the member list.
fn render_members(value: &Value) {
    println!(
        "Roster epoch {}",
        value["rosterEpoch"].as_u64().unwrap_or(0)
    );

    for member in value["members"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[])
    {
        // Role and state are words rather than colours, so a terminal
        // without colour shows the same information (PRD section 27).
        println!(
            "{}  {}  {}",
            if member["active"].as_bool() == Some(true) {
                "active  "
            } else {
                "removed "
            },
            if member["administrator"].as_bool() == Some(true) {
                "admin "
            } else {
                "member"
            },
            member["principalId"].as_str().unwrap_or("?")
        );

        for device in member["devices"]
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or(&[])
        {
            println!(
                "    {}  {}",
                if device["active"].as_bool() == Some(true) {
                    "active "
                } else {
                    "revoked"
                },
                device["deviceId"].as_str().unwrap_or("?")
            );
        }
    }
}

/// Renders this principal's devices.
fn render_devices(value: &Value) {
    let devices = value["devices"]
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    if devices.is_empty() {
        println!("No devices in this channel.");
        return;
    }

    for device in devices {
        println!(
            "{}  {}  added in epoch {}",
            if device["active"].as_bool() == Some(true) {
                "active "
            } else {
                "revoked"
            },
            device["deviceId"].as_str().unwrap_or("?"),
            device["addedInEpoch"].as_u64().unwrap_or(0)
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_fields_do_not_panic() {
        // Rendering is the last thing to run before output; it must not be
        // the thing that crashes a command that otherwise succeeded.
        let empty = json!({});

        assert_eq!(text(&empty, "missing"), "");
        assert_eq!(optional(&empty, "missing", "never"), "never");
        assert_eq!(number(&empty, "missing"), 0);
        assert!(array(&empty, "missing").is_empty());
    }

    #[test]
    fn a_null_field_falls_back() {
        let value = json!({ "lastFetchedRevision": Value::Null });
        assert_eq!(optional(&value, "lastFetchedRevision", "never"), "never");
    }
}
