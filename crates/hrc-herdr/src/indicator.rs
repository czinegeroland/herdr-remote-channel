//! The ambient indicator: what this channel is claiming of a person's
//! attention, on surfaces that are visible when the inbox is not.
//!
//! PRD section 23.1 described a sidebar line and open question OQ-015 could
//! not find a surface to put it on; decision DEC-096 settled on the inbox
//! pane's own bottom border, which is correct and remains so. It is also not
//! the whole answer, because the border is only visible to somebody already
//! looking at the pane.
//!
//! Herdr 0.9.1 has two surfaces a plugin may write a *number* to, both
//! checked against the running host rather than assumed:
//!
//! - `pane.report_metadata` attaches tokens to a pane, which Herdr shows in
//!   its own sidebar beside that pane.
//! - `client.window_title.set` sets the terminal window title, which the
//!   operating system shows when Herdr is not the focused window.
//!
//! Neither can carry a halt *reason* — a token is a short value in someone
//! else's sidebar and a window title is a strip of text an operating system
//! may truncate at will — so DEC-096 is unchanged and the sentence stays on
//! the border. What these carry is the count, which is the part that has to
//! reach somebody who is looking somewhere else.
//!
//! Nothing here is sender-chosen. Counts, fixed local words, and nothing
//! from an envelope, for the reason section 23.3 gives about notifications:
//! this text leaves the pane and may be mirrored somewhere the provenance
//! framing is not.

use crate::sidebar::Sidebar;

/// What the indicator should currently say, or nothing.
///
/// `None` is a real state and is what makes this usable: a channel with
/// nothing waiting must actively stop claiming a line in somebody's sidebar
/// and a strip of their window title, rather than leaving a stale `0` behind.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Indicator {
    /// The short form for a pane token in Herdr's sidebar.
    pub token: Option<String>,
    /// The form for the terminal window title.
    pub title: Option<String>,
}

impl Indicator {
    /// Derives what to show from the section 23.1 state.
    ///
    /// A halt wins outright. Section 26 makes a tamper halt sticky and
    /// visible, and a count of unread notes is not what a person needs to
    /// read first when synchronization stopped because the published history
    /// was rewritten. The word is all that fits here; the reason is on the
    /// pane border, and this is what sends somebody to look at it.
    ///
    /// Otherwise the count that matters is the one that blocks: entries
    /// awaiting a decision. Unread notes are deliberately not promoted to an
    /// ambient surface — the same judgement section 23.3 makes when it
    /// leaves a note off the notification list.
    pub fn from_sidebar(sidebar: &Sidebar) -> Self {
        if sidebar.halted > 0 {
            return Self {
                token: Some("HALTED".into()),
                title: Some("hrc: halted".into()),
            };
        }

        if sidebar.approvals == 0 {
            return Self::default();
        }

        Self {
            token: Some(format!("{} waiting", sidebar.approvals)),
            title: Some(format!("hrc: {} waiting", sidebar.approvals)),
        }
    }
}

#[cfg(test)]
mod tests;
