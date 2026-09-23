//! The inbox side view: a metadata-only split that reloads once a second.

use super::*;

/// How often the inbox side view reloads its rows.
///
/// A second is fast enough that an arrival appears while a person is still
/// looking at the pane, and slow enough that the cost is one indexed read of
/// a local SQLite file per second. The alternative the PRD rules out is a
/// frequently fired Herdr host event, which would spawn a process per tick
/// (section 13.2).
pub(super) const INBOX_REFRESH: Duration = Duration::from_secs(1);

/// The inbox side view, wired to storage and to the Herdr host.
///
/// [`hrc_tui::InboxApp`] is the state machine and knows nothing about either.
/// This owns the database handle and the path back to Herdr, which is what
/// keeps reloading out of the UI and makes the UI testable without a
/// database.
pub(super) struct InboxScreen {
    app: hrc_tui::InboxApp,
    database: Database,
    channel_id: String,
    channel_local_name: String,
    now: String,
    /// Each message the popup was opened for, in order, so the command can
    /// report what a person actually looked at.
    reviewed: Vec<String>,
    /// What this installation was configured to do.
    config: hrc_herdr::Config,
    /// The last ambient indicator this pane sent, so an unchanged one is
    /// not resent.
    ///
    /// The pane reloads once a second and the count changes rarely, so
    /// sending on every tick would be a socket connection per second to say
    /// the same thing. `None` is "nothing has been sent yet", which differs
    /// from a sent empty indicator: the first tick of a quiet channel still
    /// has to clear whatever a previous pane left behind.
    indicated: Option<hrc_herdr::Indicator>,
}

impl InboxScreen {
    /// Reloads the rows, keeping the last good ones when the read fails.
    fn reload(&mut self) {
        match inbox_rows(&self.database, &self.channel_id, &self.channel_local_name) {
            Ok((rows, now)) => {
                self.now = now;
                self.announce(&rows);
                self.app.refresh(rows);
                // The section 23.1 indicator, on the surface it describes.
                if let Ok(health) = crate::commands::herdr_sidebar_for(&self.database) {
                    self.indicate(&health);
                    self.app.set_health(health);
                }
            }
            // A failed read must not look like an empty inbox. The rows
            // already on screen stay, and the status line says why they may
            // be out of date.
            Err(error) => self.app.refresh_failed(error.to_string()),
        }
    }

    /// Puts the count on the surfaces that are visible from elsewhere.
    ///
    /// Only when it changed. The pane reloads once a second and the number
    /// of decisions waiting changes rarely, so an unconditional send would
    /// open a socket every second to repeat itself.
    ///
    /// Failures are swallowed for the reason `announce` gives: the pane in
    /// front of the person already shows this, and failing a refresh because
    /// a sidebar token did not land would replace a missing count with a
    /// missing inbox.
    fn indicate(&mut self, health: &hrc_herdr::Sidebar) {
        if !self.config.pane_token && !self.config.window_title {
            return;
        }

        let indicator = hrc_herdr::Indicator::from_sidebar(health);
        if self.indicated.as_ref() == Some(&indicator) {
            return;
        }

        let Ok(mut host) = crate::herdr_host::Host::connect() else {
            return;
        };

        if self.config.pane_token
            && let Ok(pane_id) = std::env::var(hrc_herdr::host::PANE_ENV)
        {
            let _ = host.report_token(&pane_id, indicator.token.as_deref());
        }

        if self.config.window_title {
            let _ = host.set_window_title(indicator.title.as_deref());
        }

        // Recorded only after the attempt, so a host that was unreachable
        // for one tick is tried again on the next rather than remembered as
        // done.
        self.indicated = Some(indicator);
    }

    /// Raises a Herdr notification for anything newly actionable.
    ///
    /// The ledger decides what is new, so a pane refreshing once a second
    /// raises nothing on the refreshes where nothing changed — which is
    /// almost all of them, and is the behaviour people turn notifications
    /// off over when it is missing.
    ///
    /// A failure here is deliberately swallowed. The message is in the inbox
    /// whether or not a toast appeared, and the pane in front of the person
    /// already shows it; failing a refresh because a notification did not
    /// land would replace a missing toast with a missing inbox.
    fn announce(&mut self, rows: &[hrc_herdr::InboxRow]) {
        let Ok(notices) = raise_notifications(&self.database, &self.channel_id, rows) else {
            return;
        };

        if notices.is_empty() {
            return;
        }

        let Ok(mut host) = crate::herdr_host::Host::connect() else {
            return;
        };

        for notice in notices {
            let (Some(text), Some(urgent)) = (
                notice.get("text").and_then(Value::as_str),
                notice.get("urgent").and_then(Value::as_bool),
            ) else {
                continue;
            };

            let _ = host.notify(text, urgent);
        }
    }

    /// Asks Herdr to open the trusted review popup on one message.
    ///
    /// The side view stays open behind it. Returning the outcome from the
    /// runner instead would restore the terminal and exit, which in a Herdr
    /// split means closing the pane the person was working out of.
    fn open_review(&mut self, message_id: &str) {
        // Only a well-formed ULID is handed to the host. A `messageId`
        // arrives in the envelope and the protocol checks only that it is
        // non-empty, so it is sender-chosen text; the popup opens on the
        // unfocused pending list rather than carrying that text onto a
        // command line (decision DEC-087).
        if !hrc_protocol::is_ulid(message_id) {
            self.status("That message has a malformed identifier. Opening the full pending list.");
            self.spawn_review(None);
            return;
        }

        self.reviewed.push(message_id.to_owned());
        self.spawn_review(Some(message_id));
    }

    /// Runs `herdr plugin pane open`, reporting a refusal rather than hiding it.
    fn spawn_review(&mut self, message_id: Option<&str>) {
        let herdr = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned());
        let arguments = match message_id {
            Some(message_id) => hrc_herdr::open_review(message_id),
            None => hrc_herdr::open_review_list(),
        };

        match std::process::Command::new(&herdr)
            .args(&arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(output) if output.status.success() => {}
            // Herdr answers `ui_busy` while Settings, Copy mode, or another
            // modal is up. Saying so leaves the person able to dismiss that
            // and press Enter again; a silent failure would look like a pane
            // that ignores the key.
            Ok(output) => {
                let reason = String::from_utf8_lossy(&output.stderr);
                let reason = reason.trim();
                self.status(&format!(
                    "Herdr did not open the review popup{}{}",
                    if reason.is_empty() { "" } else { ": " },
                    reason
                ));
            }
            Err(error) => self.status(&format!("Could not run {herdr}: {error}")),
        }
    }

    /// Puts one fixed local line in front of the person.
    fn status(&mut self, text: &str) {
        self.app.refresh_failed(text.to_owned());
    }
}

impl hrc_tui::Screen for InboxScreen {
    type Outcome = hrc_tui::InboxOutcome;

    fn draw(&self, frame: &mut hrc_tui::ratatui::Frame<'_>) {
        hrc_tui::render_inbox(frame, &self.app, &self.now);
    }

    fn on_key(&mut self, key: hrc_tui::crossterm::event::KeyEvent) -> Option<Self::Outcome> {
        let outcome = self.app.on_key(key);

        if self.app.refresh_requested() {
            self.reload();
        }

        match outcome {
            Some(hrc_tui::InboxOutcome::Review { message_id }) => {
                self.open_review(&message_id);
                // Deliberately not propagated. The only outcome that ends the
                // side view is leaving it.
                None
            }
            Some(hrc_tui::InboxOutcome::Quit) => Some(hrc_tui::InboxOutcome::Quit),
            None => None,
        }
    }
}

impl hrc_tui::Ticking for InboxScreen {
    fn on_tick(&mut self) -> Option<Self::Outcome> {
        self.reload();
        None
    }
}

/// Reads the inbox rows for the one configured channel, with the time used
/// to judge expiry and render ages.
pub(super) fn inbox_rows(
    database: &Database,
    channel_id: &str,
    channel_local_name: &str,
) -> Result<(Vec<hrc_herdr::InboxRow>, String)> {
    let now = database.utc_now()?;

    // Shortened once, here, so the pane title, every row and every
    // notification agree on what this channel is called. A notification is
    // capped at eighty characters by the host, and a locator spent most of
    // them (decision DEC-095).
    let channel_local_name = &hrc_herdr::channel_display_name(channel_local_name);

    // Read once for the whole render, so every row on screen resolves its
    // sender against the same snapshot. A name that changed halfway down a
    // list would be the surface disagreeing with itself.
    let aliases = database.principal_aliases(channel_id)?;

    let rows = database
        .plugin_inbox(channel_id)?
        .iter()
        .map(|entry| {
            // The name the local human assigned when they verified this
            // person, falling back to a shortened principal ID. Both are
            // locally resolved, which is what section 19.1 requires; neither
            // is a name the sender chose, which is what it forbids.
            let sender = hrc_herdr::principal_display_name(
                &entry.sender_principal,
                aliases.get(&entry.sender_principal).map(String::as_str),
            );

            hrc_herdr::InboxRow::from_entry(entry, &sender, channel_local_name, &now)
        })
        .collect();

    Ok((hrc_herdr::InboxView::new(rows).rows, now))
}

/// Builds the inbox snapshot a non-interactive caller receives.
///
/// The same rows the side view draws, as JSON. This is the agent-safe
/// surface and stays exactly as wide as it was: metadata only, no body, no
/// field that could hold one.
pub fn inbox_snapshot(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = crate::commands::only_channel(&database)?;
    let (rows, _) = inbox_rows(&database, &channel.channel_id, &channel.local_name)?;

    let notifications = raise_notifications(&database, &channel.channel_id, &rows)?;
    let view = hrc_herdr::InboxView::new(rows);

    Ok(json!({
        "status": "ok",
        "pane": "inbox",
        "channel": channel.local_name,
        "pending": view.pending(),
        "rows": view.rows,
        "notifications": notifications,
    }))
}

/// The notices this refresh should raise, recording that it raised them.
///
/// Called on every read of the inbox, which is once a second while the side
/// view is open. What makes that safe is that the ledger is in the database
/// rather than in this process: `record_notification` returns true the first
/// time a message-and-kind pair is seen and false every time after, across
/// refreshes and across restarts.
///
/// Messages that are no longer awaiting a decision have their notices
/// resolved in the same pass. A person who declined something should not be
/// told about it again by a pane that has not caught up, and a resolved row
/// stays in the ledger so that being decided is distinguishable from never
/// having been notified.
pub(super) fn raise_notifications(
    database: &Database,
    channel_id: &str,
    rows: &[hrc_herdr::InboxRow],
) -> Result<Vec<Value>> {
    let now = database.utc_now()?;
    let mut fresh = Vec::new();
    let announcing = crate::commands::plugin_config().0.notifications;

    for row in rows {
        let Some(notification) = hrc_herdr::Notification::for_message(row) else {
            continue;
        };

        if !row.awaiting_decision() {
            database.resolve_notifications(&row.message_id, &now)?;
            continue;
        }

        // The ledger is written either way. Turning notifications off
        // silences the announcement, not the record of what this
        // installation would have announced -- so turning them back on does
        // not replay everything that arrived while they were off, which is
        // the behaviour that makes a person turn them off permanently.
        let first =
            database.record_notification(channel_id, &row.message_id, notification.kind(), &now)?;

        if first && announcing {
            fresh.push(notification);
        }
    }

    Ok(hrc_herdr::coalesce(fresh)
        .into_iter()
        .map(|notice| {
            json!({
                "text": notice.text,
                "urgent": notice.urgent,
                "covers": notice.covers,
            })
        })
        .collect())
}

/// Opens the inbox side view (PRD section 23.2).
///
/// Interactive when a person is at the terminal, and the JSON snapshot
/// otherwise. Both show the same closed metadata set, so this is a choice
/// about presentation rather than about what is disclosed: a pipe gets rows
/// it can parse, a person gets rows they can move through. Refusing the
/// non-interactive case would break the agent-safe read that section 19.1
/// exists to allow, which is the opposite of what section 22.7 protects.
pub fn inbox(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return inbox_snapshot(context);
    }

    let database = Database::open(context.paths.database())?;
    let channel = crate::commands::only_channel(&database)?;
    let (rows, now) = inbox_rows(&database, &channel.channel_id, &channel.local_name)?;

    let mut screen = InboxScreen {
        app: hrc_tui::InboxApp::new(channel.local_name.clone(), rows),
        database,
        channel_id: channel.channel_id.clone(),
        channel_local_name: channel.local_name.clone(),
        now,
        reviewed: Vec::new(),
        config: crate::commands::plugin_config().0,
        indicated: None,
    };

    hrc_tui::run_ticking(&mut screen, INBOX_REFRESH).map_err(|source| CliError::Io {
        action: "run the inbox side view",
        source,
    })?;

    Ok(json!({
        "status": "ok",
        "pane": "inbox",
        "channel": channel.local_name,
        "pending": screen.app.pending(),
        "reviewed": screen.reviewed,
    }))
}
