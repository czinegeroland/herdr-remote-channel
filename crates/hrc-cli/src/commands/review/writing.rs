//! Writing a note, question or reply, and choosing who it is for.

use super::*;

/// Opens the composition screen.
///
/// Sending is not a trusted-human operation, so this needs no authorization;
/// it needs a terminal because it is a terminal interface. The refusal is the
/// same one the approval screens give, which is a little strong for what this
/// does, but a caller that wanted to send without a terminal already has
/// `hrc send` and the agent-safe `draft` method.
pub fn compose(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "send a message")?;
    let mut recipients = addressable(context)?;
    recipients.extend(answerable(context)?);

    if recipients.is_empty() {
        return Ok(json!({
            "status": "ok",
            "recipients": 0,
            "sent": Value::Null,
        }));
    }

    let mut screen = ComposeApp::new(recipients);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the composition screen",
        source,
    })?;

    let ComposeOutcome::Send {
        recipient,
        kind,
        text,
        in_reply_to,
    } = outcome
    else {
        return Ok(json!({
            "status": "ok",
            "sent": Value::Null,
        }));
    };

    // The same functions the CLI uses, rather than a second publication
    // path. A screen that composed its own envelopes would be a second place
    // where the chain, the roster epoch and the expiry rules live.
    //
    // A reply goes through `reply`, which reads the thread from the message
    // being answered rather than taking one. The kind is not consulted there:
    // an answer is an answer.
    match in_reply_to {
        Some(message_id) => crate::commands::reply(context, &message_id, &text),
        None => match kind {
            hrc_tui::ComposeKind::Note => crate::commands::send(context, &recipient, &text, None),
            hrc_tui::ComposeKind::Question => {
                crate::commands::ask(context, &recipient, &text, None)
            }
        },
    }
}

/// Messages that can be answered, as reply targets.
///
/// Only what arrived from someone else and is still awaiting a decision: a
/// message already declined or expired is not a conversation to continue, and
/// answering one this installation sent would be answering itself.
///
/// The body is not read and could not be: an unapproved body stays sealed.
/// What the list shows is the kind and the sender, which is the agent-safe
/// metadata the inbox already displays.
pub(super) fn answerable(context: &Context) -> Result<Vec<Recipient>> {
    let database = Database::open(context.paths.database())?;
    let local_principal = crate::commands::local_identity(context)?.principal_id;

    let mut targets = Vec::new();
    for channel in database.channels()? {
        // Per channel, because an alias is recorded per channel: the same
        // key verified in two places may have been named in only one.
        let aliases = database.principal_aliases(&channel.channel_id)?;

        for entry in database.inbox_entries(&channel.channel_id)? {
            if entry.sender_principal == local_principal {
                continue;
            }

            if !matches!(
                entry.disposition.as_str(),
                "quarantined" | "approved" | "edited"
            ) {
                continue;
            }

            targets.push(Recipient {
                display_name: aliases.get(&entry.sender_principal).cloned(),
                principal_id: entry.sender_principal,
                active: true,
                answering: Some(entry.kind),
                in_reply_to: Some(entry.message_id),
            });
        }
    }

    Ok(targets)
}

/// Everyone in the roster this installation can write to.
///
/// The local principal is filtered out. It is in the roster, and leaving it
/// in would make "send to myself" the preselected first entry on a screen
/// where the first entry is preselected.
pub(super) fn addressable(context: &Context) -> Result<Vec<Recipient>> {
    let listing = crate::commands::roster_listing(context)?;
    let local_principal = crate::commands::local_identity(context)?.principal_id;

    // Read once, before the screen opens, so every row shows the name that
    // was on record when the person started looking at the roster.
    let aliases = {
        let database = Database::open(context.paths.database())?;
        database.principal_aliases(&listing.channel_id)?
    };

    Ok(listing
        .members
        .into_iter()
        .filter(|member| member.principal_id != local_principal)
        .map(|member| Recipient {
            display_name: aliases.get(&member.principal_id).cloned(),
            principal_id: member.principal_id,
            active: member.active,
            in_reply_to: None,
            answering: None,
        })
        .collect())
}
