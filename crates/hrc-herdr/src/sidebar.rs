//! The sidebar indicator (PRD section 23.1).
//!
//! One line, always the same shape, so a glance is enough:
//!
//! ```text
//! HRC: 3 unread | 1 approval | synced 12s ago
//! ```
//!
//! A halted channel takes the front of the line. Section 26 requires a
//! tamper halt to be sticky and visible, and a count of unread notes is not
//! the thing a person needs to read first when synchronization has stopped
//! because the history was rewritten.

use hrc_core::rpc::ChannelStatus;

/// What the sidebar shows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sidebar {
    /// Messages that have arrived and not yet been read.
    pub unread: usize,
    /// Entries still awaiting a local human decision.
    pub approvals: usize,
    /// Seconds since the last successful synchronization, if there was one.
    pub synced_seconds_ago: Option<u64>,
    /// Channels whose synchronization has halted.
    pub halted: usize,
    /// Questions asked of this installation that it has not answered.
    ///
    /// Distinct from `approvals`: an approval is a message waiting for a
    /// decision, and this is a decided or even delivered question that was
    /// never replied to. The second is the one that disappears quietly,
    /// because nothing is left in a pending list to remind anybody of it.
    pub unanswered: usize,
    /// Questions this installation asked that nobody has answered.
    ///
    /// Carried but not rendered on the one-line indicator. It is real and
    /// worth showing where there is room, but the line has to stay short
    /// and what a person can act on is what they owe, not what they are
    /// owed.
    pub awaiting_answer: usize,
}

impl Sidebar {
    /// Summarizes the daemon's per-channel status.
    ///
    /// Unread is not derivable from [`ChannelStatus`], so it is passed in by
    /// the caller that knows it rather than guessed at here.
    pub fn from_status(
        channels: &[ChannelStatus],
        unread: usize,
        synced_seconds_ago: Option<u64>,
        unanswered: usize,
        awaiting_answer: usize,
    ) -> Self {
        Self {
            unread,
            approvals: channels.iter().map(|channel| channel.pending).sum(),
            synced_seconds_ago,
            halted: channels.iter().filter(|channel| channel.halted).count(),
            unanswered,
            awaiting_answer,
        }
    }

    /// Renders the one-line indicator.
    pub fn render(&self) -> String {
        let mut parts = Vec::with_capacity(4);

        if self.halted > 0 {
            parts.push(format!("{} HALTED", self.halted));
        }

        parts.push(format!("{} unread", self.unread));
        parts.push(plural(self.approvals, "approval"));
        if self.unanswered > 0 {
            parts.push(format!("{} unanswered", self.unanswered));
        }
        parts.push(match self.synced_seconds_ago {
            Some(seconds) => format!("synced {} ago", elapsed(seconds)),
            None => "never synced".to_owned(),
        });

        format!("HRC: {}", parts.join(" | "))
    }
}

/// `1 approval`, `2 approvals`.
fn plural(count: usize, noun: &str) -> String {
    if count == 1 {
        format!("{count} {noun}")
    } else {
        format!("{count} {noun}s")
    }
}

/// A duration short enough to sit in a sidebar.
///
/// Deliberately coarse. The exact age of the last fetch is not a decision
/// input; whether it was seconds or days ago is.
fn elapsed(seconds: u64) -> String {
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3599 => format!("{}m", seconds / 60),
        3600..=86_399 => format!("{}h", seconds / 3600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests;
