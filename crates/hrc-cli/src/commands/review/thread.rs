//! The thread pane: one conversation, read top to bottom (docs/RESEARCH.md
//! 6.4).
//!
//! The rule is the one `hrc show` already follows. A message's content
//! appears only when a human released it by delivering it; everything else
//! appears as metadata and a fixed local line saying why there is no
//! content. Messages this installation sent appear as metadata only,
//! because their plaintext is sealed to their recipients and is not kept
//! here in readable form.

use super::*;

/// What a quarantined message says in place of its body.
pub(crate) const AWAITING: &str =
    "Not yet approved. Select it in the inbox and press Enter to review it.";

/// What a message decided without delivery says in place of its body.
pub(crate) const NOT_RELEASED: &str =
    "Kept or declined without releasing its content, so there is nothing to show.";

/// What a message this installation sent says in place of its body.
pub(crate) const SENT_HERE: &str = "Sent from here. Its content is sealed to its recipients and is not kept here in readable form.";

/// Opens the thread pane.
///
/// `target` is the message the inbox opened it on. Without one it opens the
/// thread of the most recent arrival, which is what choosing the action
/// directly most likely means. Not at a terminal, it returns the same
/// agent-safe JSON as `hrc thread`.
pub fn thread_view(context: &Context, target: Option<&str>) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = crate::commands::only_channel(&database)?;
    let inbox = database.inbox_entries(&channel.channel_id)?;

    let chosen = match target {
        Some(message_id) => inbox.iter().find(|entry| entry.message_id == message_id),
        None => inbox.iter().max_by_key(|entry| entry.arrival_sequence),
    };
    let thread_id = chosen
        .map(|entry| {
            entry
                .thread_id
                .clone()
                .unwrap_or_else(|| entry.message_id.clone())
        })
        .or_else(|| target.map(str::to_owned));

    let Some(thread_id) = thread_id else {
        return Ok(json!({ "status": "ok", "pane": "thread", "entries": 0 }));
    };

    if !is_a_terminal() {
        return crate::commands::thread(context, &thread_id);
    }

    let entries = thread_entries(&database, &channel.channel_id, &thread_id)?;
    let count = entries.len();
    let title = format!(
        "Thread in {} ({count} message{})",
        hrc_herdr::channel_display_name(&channel.local_name),
        if count == 1 { "" } else { "s" }
    );

    let mut screen = hrc_tui::ThreadApp::new(title, entries);
    hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the thread view",
        source,
    })?;

    Ok(json!({ "status": "ok", "pane": "thread", "entries": count }))
}

/// Every message in one thread, in reading order.
///
/// The order is this installation's own: when each message arrived here or
/// was written here, from [`Database::thread_order`]. It is never the
/// sender-chosen creation time or anything derived from it, such as a ULID
/// identifier's time prefix, because that would let a remote peer place its
/// message anywhere in this reading of the conversation (section 18.2,
/// decision DEC-042).
pub(crate) fn thread_entries(
    database: &Database,
    channel_id: &str,
    thread_id: &str,
) -> Result<Vec<hrc_tui::ThreadEntry>> {
    let aliases = database.principal_aliases(channel_id)?;

    let inbound: std::collections::HashMap<String, hrc_storage::InboxEntry> = database
        .thread_entries(channel_id, thread_id)?
        .into_iter()
        .map(|entry| (entry.message_id.clone(), entry))
        .collect();
    let outbound: std::collections::HashMap<String, String> = database
        .sent_messages(channel_id)?
        .into_iter()
        .filter(|sent| sent.thread_id == thread_id)
        .map(|sent| (sent.message_id, sent.kind))
        .collect();

    let mut entries = Vec::new();
    for place in database.thread_order(channel_id, thread_id)? {
        if place.outbound {
            // A message still being built has no recorded kind or recipients
            // yet, and is not in the sent facts; it appears once it is.
            if let Some(kind) = outbound.get(&place.message_id) {
                entries.push(hrc_tui::ThreadEntry {
                    heading: format!("you · {kind}"),
                    body: SENT_HERE.to_owned(),
                });
            }
            continue;
        }

        let Some(entry) = inbound.get(&place.message_id) else {
            continue;
        };
        let sender = hrc_herdr::principal_display_name(
            &entry.sender_principal,
            aliases.get(&entry.sender_principal).map(String::as_str),
        );
        let body = match crate::commands::read::released_content(database, &entry.message_id)? {
            Some(content) => content,
            None if entry.disposition == "quarantined" => AWAITING.to_owned(),
            None => NOT_RELEASED.to_owned(),
        };

        entries.push(hrc_tui::ThreadEntry {
            heading: format!("{sender} · {}", entry.kind),
            body,
        });
    }

    Ok(entries)
}

/// The message the thread pane was opened for, if Herdr passed a valid one.
pub fn thread_target() -> Option<String> {
    review_target_from(std::env::var(hrc_herdr::THREAD_TARGET_ENV).ok())
}
