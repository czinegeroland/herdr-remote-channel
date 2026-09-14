//! Herdr events delivered to `bin/hrc herdr event` (PRD section 13.2).
//!
//! Herdr does not write the event to standard input. It runs the hook's argv
//! with the plugin directory as the working directory and names the event in
//! `HERDR_PLUGIN_EVENT`, with the host's payload in `HERDR_PLUGIN_EVENT_JSON`.
//! So the hook dispatches on the *name*, which is part of the host's
//! documented contract, and never parses the payload: nothing this plugin
//! does with an event needs a field out of it, and a parser for a shape we
//! have not verified would fail on host input that is perfectly valid.
//!
//! Two things shape the rest.
//!
//! First, the subscription list is deliberately one entry long. Section 13.2
//! says frequently occurring event processing belongs in the daemon rather
//! than in repeatedly spawned hook commands, and every name added here is a
//! process spawn at the host's rate, not ours.
//!
//! Second, an event never carries a decision. Section 19.2 says approval is a
//! human act on the trusted screen, so the reactions below refresh and
//! nothing else — there is no variant that could approve, deliver, or reveal
//! anything, and that is a property of the type rather than of a handler.

use serde::Serialize;

/// The name of the environment variable Herdr names the event in.
pub const EVENT_VARIABLE: &str = "HERDR_PLUGIN_EVENT";

/// A host event this plugin subscribes to.
///
/// A closed set, and the same one the manifest declares: `manifest()` builds
/// its `[[events]]` tables from [`HostEvent::ALL`], so a name this plugin can
/// react to and a name it is registered for cannot disagree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostEvent {
    /// A person brought a workspace to the front.
    ///
    /// The one moment where a stale sidebar is visible to somebody. Workspace
    /// and tab creation, closure, renaming and movement all leave the channel
    /// state untouched, so subscribing to them would spawn a process to
    /// recompute an answer that has not changed.
    WorkspaceFocused,
}

impl HostEvent {
    /// Every event this plugin subscribes to, in manifest order.
    pub const ALL: [HostEvent; 1] = [HostEvent::WorkspaceFocused];

    /// The host's name for this event.
    pub const fn as_str(self) -> &'static str {
        match self {
            HostEvent::WorkspaceFocused => "workspace.focused",
        }
    }

    /// Parses a host event name, returning `None` for anything else.
    ///
    /// Anything else includes events this plugin is not registered for. A
    /// host may deliver one — a shared subscription, a later Herdr that
    /// widens a category — and reacting to an event we did not ask for is
    /// how a hook ends up running at a rate nobody chose.
    pub fn parse(name: &str) -> Option<HostEvent> {
        HostEvent::ALL
            .into_iter()
            .find(|event| event.as_str() == name.trim())
    }

    /// What the plugin does about this event.
    pub const fn reaction(self) -> Reaction {
        match self {
            HostEvent::WorkspaceFocused => Reaction::RefreshStatus,
        }
    }
}

/// What the plugin does about an event.
///
/// No variant approves, delivers, or reveals anything. That is not an
/// oversight to be filled in later: an event is a host callback, and a host
/// callback that could approve remote content would be an approval no human
/// made.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "reaction", rename_all = "snake_case")]
pub enum Reaction {
    /// Ask the daemon for current state and redraw the sidebar.
    RefreshStatus,
    /// Nothing to do.
    Ignore,
}

/// The reaction to whatever the host named, including nothing at all.
///
/// A hook invoked with no event named is not an error: Herdr runs the same
/// argv for several purposes, and exiting non-zero because a variable was
/// absent would show a person a failed plugin for a reason that is not one.
pub fn reaction_to(name: Option<&str>) -> Reaction {
    match name.and_then(HostEvent::parse) {
        Some(event) => event.reaction(),
        None => Reaction::Ignore,
    }
}

#[cfg(test)]
mod tests;
