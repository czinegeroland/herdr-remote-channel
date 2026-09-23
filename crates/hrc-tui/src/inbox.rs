//! The metadata-only inbox side view.
//!
//! This is the only long-lived surface the plugin draws, and it is the one
//! that must never show a body. Herdr keeps it as a split, so it sits in the
//! tiled layout while a person works; anything visible in it is visible for
//! as long as that split is open, to anyone who can see the screen and to
//! any agent sharing the workspace.
//!
//! So the separation this module enforces is not a convenience. [`InboxApp`]
//! holds [`hrc_herdr::InboxRow`]s, which carry the closed metadata set of PRD
//! section 19.1 and nothing else, and its outcome type has exactly two
//! variants: open the trusted review popup for one message, or leave. There
//! is no key that reveals, approves, declines, or delivers, and no variant
//! that could carry a decision — not because the screen declines to offer
//! one, but because [`InboxOutcome`] cannot express one. Revealing a body
//! stays with `crate::app::App`, behind a popup, where the pane identity
//! itself tells a reviewer that remote content may be on screen.
//!
//! Selection is tracked by message identifier rather than by row index. The
//! side view reloads while it is open, and a reload may insert a new arrival,
//! drop an expired row, or reorder what is left; an index would then point at
//! a different message than the one the person was looking at, and `Enter`
//! would open the wrong body.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use hrc_herdr::{InboxDisposition, InboxRow};

/// Which rows the side view is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InboxFilter {
    /// Only rows still awaiting a human decision.
    Pending,
    /// Only rows this installation has not opened yet.
    Unread,
    /// Everything the channel holds.
    All,
}

impl InboxFilter {
    /// The fixed local word shown in the title.
    pub const fn as_str(self) -> &'static str {
        match self {
            InboxFilter::Pending => "pending",
            InboxFilter::Unread => "unread",
            InboxFilter::All => "all",
        }
    }

    /// Whether a row belongs in this filter.
    fn admits(self, row: &InboxRow) -> bool {
        match self {
            InboxFilter::Pending => row.disposition == InboxDisposition::Pending,
            // Durable read state is not recorded yet, so "unread" is defined
            // as what has not been decided and has not expired. Saying that
            // plainly beats inventing a read flag the database cannot back:
            // the transition that marks a row read has to be written down in
            // the PRD before a surface can claim to know it (section 23.2).
            InboxFilter::Unread => matches!(
                row.disposition,
                InboxDisposition::Pending | InboxDisposition::Unsupported
            ),
            InboxFilter::All => true,
        }
    }
}

/// What the side view can ask for.
///
/// Two variants, and neither is a decision. This type is the security
/// boundary of the pane: a reviewer can read it and know that no sequence of
/// key presses in the inbox can approve, decline, reveal, or deliver
/// anything, without reading the key handler at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxOutcome {
    /// Open the trusted review popup, focused on one message.
    Review {
        /// The stored identifier of the selected row.
        message_id: String,
    },
    /// Close the side view.
    Quit,
}

/// The default status line, repeated after a transient message clears.
/// What the footer says by default.
///
/// Short on purpose. The full list is sixty-three characters, which wrapped
/// onto a second line in any pane narrower than a half-window and pushed the
/// list up; a side view that costs two rows to say what the keys are has
/// spent them badly. `?` swaps in the rest.
const KEYS: &str = "Enter review \u{b7} p/u/a filter \u{b7} ? keys";

/// The full list, once someone asks for it.
const KEYS_FULL: &str = "Enter: review  R: refresh  p: pending  u: unread  a: all  q: close";

/// Shown between asking for a reload and the reload finishing.
const REFRESHING: &str = "Refreshing...";

/// The age column for a row whose arrival time this build cannot parse.
///
/// `arrival_at` is written by this installation through
/// `Database::utc_now`, so a value that does not parse is a local fault
/// rather than sender-chosen text. It still gets a fixed label instead of
/// being printed raw, because the rule the side view keeps is about what
/// reaches the screen, not about who is at fault for it.
const UNKNOWN_AGE: &str = "  ?";

/// How long ago a row arrived, in the three characters the column has.
///
/// `arrival_at` and `now` are the fixed `YYYY-MM-DDTHH:MM:SSZ` shape that
/// `Database::utc_now` writes. Parsed by position rather than with a
/// date-time crate: the shape is this project's own, the arithmetic is
/// days-from-civil, and adding a dependency to print `2m` in a sidebar would
/// be the larger change.
pub fn age(arrival_at: &str, now: &str) -> String {
    let (Some(arrived), Some(current)) = (
        hrc_core::time::epoch_seconds(arrival_at),
        hrc_core::time::epoch_seconds(now),
    ) else {
        return UNKNOWN_AGE.to_owned();
    };

    // A row that claims to have arrived in the future is a clock that moved,
    // not a negative age.
    hrc_core::time::short_duration(current.saturating_sub(arrived).max(0) as u64)
}

/// The inbox side view.
#[derive(Debug, Clone)]
pub struct InboxApp {
    rows: Vec<InboxRow>,
    selected_message_id: Option<String>,
    filter: InboxFilter,
    status: String,
    channel_local_name: String,
    /// Set when the last reload failed, so the rows on screen can be labelled
    /// as stale without being thrown away.
    stale: bool,
    /// Set by the refresh key and cleared by the runner that performs it.
    refresh_requested: bool,
    /// How the channel itself is doing, when the caller knows.
    health: Option<hrc_herdr::Sidebar>,
}

impl InboxApp {
    /// Builds the side view over a first snapshot.
    pub fn new(channel_local_name: impl Into<String>, rows: Vec<InboxRow>) -> Self {
        let mut app = Self {
            rows,
            selected_message_id: None,
            filter: InboxFilter::Pending,
            status: KEYS.to_owned(),
            channel_local_name: channel_local_name.into(),
            stale: false,
            refresh_requested: false,
            health: None,
        };
        app.settle_selection(None);
        app
    }

    /// The rows the current filter admits, in order.
    pub fn visible(&self) -> Vec<&InboxRow> {
        self.rows
            .iter()
            .filter(|row| self.filter.admits(row))
            .collect()
    }

    /// Whether nothing has arrived at all, whatever the filter shows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The selected row, if the filter still shows one.
    pub fn selected(&self) -> Option<&InboxRow> {
        let selected = self.selected_message_id.as_deref()?;
        self.visible()
            .into_iter()
            .find(|row| row.message_id == selected)
    }

    /// The stored identifier of the selected row.
    pub fn selected_message_id(&self) -> Option<&str> {
        self.selected().map(|row| row.message_id.as_str())
    }

    /// Which rows are on show.
    pub fn filter(&self) -> InboxFilter {
        self.filter
    }

    /// The channel this side view belongs to.
    pub fn channel_local_name(&self) -> &str {
        &self.channel_local_name
    }

    /// The line telling the human what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Whether the rows on screen are from a reload that then failed.
    pub fn stale(&self) -> bool {
        self.stale
    }

    /// How many rows still await a human decision, across every filter.
    pub fn pending(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.disposition == InboxDisposition::Pending)
            .count()
    }

    /// Replaces the rows with a fresh snapshot, keeping the selection.
    ///
    /// The selected message is looked up again by identifier. If it is still
    /// there it stays selected wherever it moved to; if it is gone — decided
    /// elsewhere, expired, filtered out — the nearest row at or after its old
    /// position takes the selection, which is what a person reaching for the
    /// next item expects and is deterministic rather than "whatever index 0
    /// now holds".
    pub fn refresh(&mut self, rows: Vec<InboxRow>) {
        let previous = self.visible_position();
        self.rows = rows;
        self.stale = false;
        self.refresh_requested = false;
        if self.status == REFRESHING {
            self.status = KEYS.to_owned();
        }
        self.settle_selection(previous);
    }

    /// Reports that a reload failed, keeping the last good rows on screen.
    ///
    /// An empty inbox and an inbox that could not be read look identical if
    /// the failure is swallowed, and they mean opposite things. The rows
    /// stay, the status line says what happened, and the title says the rows
    /// are stale.
    pub fn refresh_failed(&mut self, reason: impl Into<String>) {
        self.stale = true;
        self.refresh_requested = false;
        self.status = format!(
            "Refresh failed: {}. Showing the last rows read.",
            reason.into()
        );
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<InboxOutcome> {
        // Windows delivers a release event for every press. Acting on both
        // would move the selection twice per key and, once `Enter` opens a
        // popup, would open it twice.
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(InboxOutcome::Quit);
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(InboxOutcome::Quit),
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                None
            }
            KeyCode::Char('p') => {
                self.set_filter(InboxFilter::Pending);
                None
            }
            KeyCode::Char('u') => {
                self.set_filter(InboxFilter::Unread);
                None
            }
            KeyCode::Char('a') => {
                self.set_filter(InboxFilter::All);
                None
            }
            KeyCode::Char('R') | KeyCode::Char('r') => {
                // The reload itself belongs to the caller, which owns the
                // database handle. This only records that one was asked for.
                self.refresh_requested = true;
                self.status = REFRESHING.to_owned();
                None
            }
            KeyCode::Char('?') => {
                // A toggle, so the same key puts the list away again.
                self.status = if self.status == KEYS_FULL {
                    KEYS.to_owned()
                } else {
                    KEYS_FULL.to_owned()
                };
                None
            }
            KeyCode::Enter => match self.selected_message_id() {
                Some(message_id) => Some(InboxOutcome::Review {
                    message_id: message_id.to_owned(),
                }),
                None => {
                    self.status = "Nothing selected.".to_owned();
                    None
                }
            },
            _ => None,
        }
    }

    /// Records how the channel is doing, for the line on the bottom border.
    pub fn set_health(&mut self, health: hrc_herdr::Sidebar) {
        self.health = Some(health);
    }

    /// The channel's state, in one line.
    ///
    /// This is the PRD section 23.1 indicator. It was computed by
    /// `hrc herdr event` and handed to a host surface that does not exist —
    /// Herdr's manifest declares no sidebar — so it went nowhere for as long
    /// as it has existed. The split is the surface it was describing all
    /// along: a long-lived side view of one channel (decision DEC-096).
    ///
    /// A halted channel takes the whole line. Section 26 makes a tamper halt
    /// sticky and visible, and a count of unread notes is not what a person
    /// needs to read first when synchronization stopped because the history
    /// was rewritten.
    pub fn health_line(&self) -> String {
        if self.stale {
            return "COULD NOT REFRESH".to_owned();
        }

        let Some(health) = &self.health else {
            return String::new();
        };

        if health.halted > 0 {
            return "HALTED: published history was rewritten".to_owned();
        }

        let mut parts = Vec::with_capacity(4);
        if self.pending() > 0 {
            parts.push(format!("{} waiting on you", self.pending()));
        }
        if health.unread > 0 {
            parts.push(format!("{} unread", health.unread));
        }
        // A question already delivered and never answered leaves nothing in
        // a pending list, so this is the only place it appears at all.
        if health.unanswered > 0 {
            parts.push(format!("{} unanswered", health.unanswered));
        }
        parts.push(match health.synced_seconds_ago {
            Some(seconds) => format!("synced {} ago", hrc_core::time::short_duration(seconds)),
            None => "never synced".to_owned(),
        });

        parts.join(" \u{b7} ")
    }

    /// Whether a key press asked for a reload that has not happened yet.
    ///
    /// Read by the runner after `on_key` returns `None`, so that the reload
    /// happens where the database handle lives rather than in here.
    pub fn refresh_requested(&self) -> bool {
        self.refresh_requested
    }

    /// Changes which rows are shown, keeping the selection when it survives.
    fn set_filter(&mut self, filter: InboxFilter) {
        if self.filter == filter {
            return;
        }
        let previous = self.visible_position();
        self.filter = filter;
        self.status = KEYS.to_owned();
        self.settle_selection(previous);
    }

    /// Where the selected row sits in the visible list right now.
    fn visible_position(&self) -> Option<usize> {
        let selected = self.selected_message_id.as_deref()?;
        self.visible()
            .iter()
            .position(|row| row.message_id == selected)
    }

    /// Puts the selection on a row that exists.
    ///
    /// `fallback` is where the previously selected row used to sit. When it
    /// is gone, the row now at that position takes the selection, and the
    /// last row when the list shrank past it.
    fn settle_selection(&mut self, fallback: Option<usize>) {
        let visible: Vec<String> = self
            .visible()
            .into_iter()
            .map(|row| row.message_id.clone())
            .collect();

        if visible.is_empty() {
            self.selected_message_id = None;
            return;
        }

        if let Some(selected) = self.selected_message_id.as_deref()
            && visible.iter().any(|id| id == selected)
        {
            return;
        }

        let index = fallback.unwrap_or(0).min(visible.len() - 1);
        self.selected_message_id = Some(visible[index].clone());
    }

    /// Moves the selection one row.
    fn move_selection(&mut self, delta: isize) {
        let visible: Vec<String> = self
            .visible()
            .into_iter()
            .map(|row| row.message_id.clone())
            .collect();

        if visible.is_empty() {
            return;
        }

        let current = self
            .selected_message_id
            .as_deref()
            .and_then(|selected| visible.iter().position(|id| id == selected))
            .unwrap_or(0);

        let next = if delta < 0 {
            current.saturating_sub(1)
        } else {
            (current + 1).min(visible.len() - 1)
        };

        self.selected_message_id = Some(visible[next].clone());
        self.status = KEYS.to_owned();
    }
}

#[cfg(test)]
mod tests;
