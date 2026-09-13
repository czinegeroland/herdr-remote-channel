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
        _ => println!("{value}"),
    }
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
