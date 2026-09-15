//! The panes the plugin renders (PRD section 13.2, `hrc herdr pane inbox`).
//!
//! A pane name arrives on the command line, which in a plugin host means it
//! arrives from a manifest that a person may have edited. Parsing it against
//! a closed set rather than dispatching on the string is what keeps an
//! unknown pane a clean usage error instead of an empty screen.

use serde::Serialize;

/// A pane this plugin knows how to draw.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pane {
    /// The inbox, listing what has arrived and what awaits a decision.
    Inbox,
    /// The channel setup screen: create a channel, invite someone, or
    /// redeem an invite.
    Setup,
    /// The context disclosure screen, where a package is previewed and sent.
    Context,
    /// The composition screen, for writing a note or a question without
    /// leaving Herdr.
    Compose,
    /// The membership approval screen, where a join request is checked
    /// against its safety phrase and admitted or refused.
    Joins,
    /// The trusted approval screen, where a quarantined body may be read and
    /// decided on.
    ///
    /// Separate from the inbox rather than a mode of it, because the two
    /// panes have opposite rules: the inbox is safe to leave on screen and
    /// never shows a body, while this one exists to show one. A pane that
    /// could become either depending on a key press would make "is a body
    /// visible right now" a question about history rather than about which
    /// pane is open.
    Review,
}

impl Pane {
    /// Every pane, in the order the manifest declares them.
    pub const ALL: [Pane; 6] = [
        Pane::Inbox,
        Pane::Setup,
        Pane::Compose,
        Pane::Context,
        Pane::Joins,
        Pane::Review,
    ];

    /// The name used on the command line and in the manifest.
    pub const fn as_str(self) -> &'static str {
        match self {
            Pane::Inbox => "inbox",
            Pane::Setup => "setup",
            Pane::Compose => "compose",
            Pane::Context => "context",
            Pane::Joins => "joins",
            Pane::Review => "review",
        }
    }

    /// Parses a pane name, returning `None` for anything unknown.
    pub fn parse(value: &str) -> Option<Pane> {
        Pane::ALL.into_iter().find(|pane| pane.as_str() == value)
    }
}

#[cfg(test)]
mod tests;
