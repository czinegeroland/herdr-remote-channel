//! Herdr events delivered to `hrc herdr event` (PRD section 13.2).
//!
//! Herdr writes one JSON object to standard input and the plugin decides
//! what to do about it. Two things shape this module.
//!
//! First, the input is parsed into a closed enum with unknown fields
//! rejected. An event is host input, not remote input, but the plugin should
//! still fail loudly on an event shape it does not understand rather than
//! silently treating it as something else.
//!
//! Second, an event never carries a decision. PRD section 13.2 says
//! frequently occurring event processing belongs in the daemon rather than
//! in repeatedly spawned hook commands, and section 19.2 says approval is a
//! human act on the trusted screen. So the reactions below refresh, notify,
//! and nudge the daemon — none of them approve anything, and there is no
//! variant that could.

use serde::{Deserialize, Serialize};

/// Something Herdr told the plugin about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "event",
    content = "params",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub enum Event {
    /// Herdr finished starting up.
    Started,
    /// The user opened a pane belonging to this plugin.
    PaneOpened {
        /// Which pane, as the manifest named it.
        pane: String,
    },
    /// A periodic tick the host schedules.
    Tick,
    /// Herdr is shutting down.
    Stopping,
}

/// What the plugin does about an event.
///
/// No variant approves, delivers, or reveals anything. That is not an
/// oversight to be filled in later: an event is a host callback, and a host
/// callback that could approve remote content would be an approval no human
/// made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "reaction", rename_all = "snake_case")]
pub enum Reaction {
    /// Ask the daemon for current state and redraw the sidebar.
    RefreshStatus,
    /// Redraw one pane.
    RefreshPane {
        /// Which pane.
        pane: crate::pane::Pane,
    },
    /// Nothing to do.
    Ignore,
}

impl Event {
    /// Parses one event from the JSON Herdr wrote to standard input.
    pub fn parse(input: &str) -> Result<Self, String> {
        serde_json::from_str(input).map_err(|error| error.to_string())
    }

    /// What the plugin should do about this event.
    pub fn reaction(&self) -> Reaction {
        match self {
            Event::Started | Event::Tick => Reaction::RefreshStatus,
            Event::PaneOpened { pane } => match crate::pane::Pane::parse(pane) {
                Some(pane) => Reaction::RefreshPane { pane },
                // A pane this plugin does not own is not this plugin's to
                // redraw. Herdr may host others.
                None => Reaction::Ignore,
            },
            // Shutdown is the daemon's business, and the daemon outlives the
            // plugin process. There is nothing for a hook invocation to do.
            Event::Stopping => Reaction::Ignore,
        }
    }
}

#[cfg(test)]
mod tests;
