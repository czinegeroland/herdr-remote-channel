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
}

impl Pane {
    /// Every pane, in the order the manifest declares them.
    pub const ALL: [Pane; 1] = [Pane::Inbox];

    /// The name used on the command line and in the manifest.
    pub const fn as_str(self) -> &'static str {
        match self {
            Pane::Inbox => "inbox",
        }
    }

    /// Parses a pane name, returning `None` for anything unknown.
    pub fn parse(value: &str) -> Option<Pane> {
        Pane::ALL.into_iter().find(|pane| pane.as_str() == value)
    }
}

#[cfg(test)]
mod tests;
