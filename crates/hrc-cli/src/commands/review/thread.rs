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
pub(crate) fn thread_entries(
    database: &Database,
    channel_id: &str,
    thread_id: &str,
) -> Result<Vec<hrc_tui::ThreadEntry>> {
    let aliases = database.principal_aliases(channel_id)?;

    // Sorted by message identifier where it is a ULID, whose leading
    // characters are its creation time, so replies interleave with what they
    // answer. An inbound identifier is sender-chosen and only checked for
    // shape, so this orders the reading and decides nothing else; one that is
    // not a ULID sorts after the rest, in arrival order.
    let mut keyed: Vec<((bool, String, u64), hrc_tui::ThreadEntry)> = Vec::new();

    for entry in database.thread_entries(channel_id, thread_id)? {
        let sender = hrc_herdr::principal_display_name(
            &entry.sender_principal,
            aliases.get(&entry.sender_principal).map(String::as_str),
        );
        let body = match crate::commands::read::released_content(database, &entry.message_id)? {
            Some(content) => content,
            None if entry.disposition == "quarantined" => AWAITING.to_owned(),
            None => NOT_RELEASED.to_owned(),
        };

        keyed.push((
            order_key(&entry.message_id, entry.arrival_sequence),
            hrc_tui::ThreadEntry {
                heading: format!("{sender} · {}", entry.kind),
                body,
            },
        ));
    }

    for sent in database
        .sent_messages(channel_id)?
        .into_iter()
        .filter(|sent| sent.thread_id == thread_id)
    {
        keyed.push((
            order_key(&sent.message_id, u64::MAX),
            hrc_tui::ThreadEntry {
                heading: format!("you · {}", sent.kind),
                body: SENT_HERE.to_owned(),
            },
        ));
    }

    keyed.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(keyed.into_iter().map(|(_, entry)| entry).collect())
}

/// Where a message sorts: ULIDs by their time-ordered text, the rest after.
fn order_key(message_id: &str, arrival: u64) -> (bool, String, u64) {
    if hrc_protocol::is_ulid(message_id) {
        (false, message_id.to_owned(), arrival)
    } else {
        (true, String::new(), arrival)
    }
}

/// The message the thread pane was opened for, if Herdr passed a valid one.
pub fn thread_target() -> Option<String> {
    review_target_from(std::env::var(hrc_herdr::THREAD_TARGET_ENV).ok())
}
