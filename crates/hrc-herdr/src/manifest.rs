//! The Herdr plugin manifest (PRD section 13.2).
//!
//! Herdr's manifest actions, events, panes, and startup entries all invoke
//! the same `hrc` binary, so the manifest is really a list of argument
//! vectors. Generating it here rather than checking in a hand-written file
//! means the entry points the manifest advertises and the subcommands the
//! binary accepts are produced from the same source, and a test can hold
//! them together.

use serde::Serialize;

/// One thing Herdr can invoke.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Entry {
    /// Stable identifier Herdr uses to refer to this entry.
    ///
    /// Namespaced by kind. An action and a pane can both be "the inbox", and
    /// a manifest where two entries answer to the same name is one where the
    /// host picks for you.
    pub id: String,
    /// What a person sees in the Herdr interface.
    pub title: String,
    /// The arguments passed to the `hrc` binary.
    pub command: Vec<String>,
}

/// The plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// Plugin identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// One-line description.
    pub description: String,
    /// The executable every entry invokes.
    pub executable: String,
    /// Run once when Herdr starts.
    pub startup: Entry,
    /// Actions a person can trigger.
    pub actions: Vec<Entry>,
    /// The event hook Herdr calls with a JSON event on standard input.
    pub events: Entry,
    /// Panes this plugin draws.
    pub panes: Vec<Entry>,
}

/// The manifest this build advertises.
pub fn manifest() -> Manifest {
    Manifest {
        id: "herdr-remote-channel".to_owned(),
        name: "Herdr Remote Channel".to_owned(),
        description: "Secure asynchronous communication between independent Herdr installations."
            .to_owned(),
        executable: "hrc".to_owned(),
        startup: Entry {
            id: "startup".to_owned(),
            title: "Start Herdr Remote Channel".to_owned(),
            command: vec!["herdr".to_owned(), "startup".to_owned()],
        },
        actions: vec![Entry {
            id: "inbox".to_owned(),
            title: "Remote channel inbox".to_owned(),
            command: vec!["herdr".to_owned(), "action".to_owned(), "inbox".to_owned()],
        }],
        events: Entry {
            id: "event".to_owned(),
            title: "Herdr Remote Channel event hook".to_owned(),
            command: vec!["herdr".to_owned(), "event".to_owned()],
        },
        panes: crate::pane::Pane::ALL
            .into_iter()
            .map(|pane| Entry {
                id: format!("pane.{}", pane.as_str()),
                title: match pane {
                    crate::pane::Pane::Inbox => "Remote channel inbox".to_owned(),
                },
                command: vec![
                    "herdr".to_owned(),
                    "pane".to_owned(),
                    pane.as_str().to_owned(),
                ],
            })
            .collect(),
    }
}

impl Manifest {
    /// Every entry, for callers that need to check them uniformly.
    pub fn entries(&self) -> Vec<&Entry> {
        let mut entries = vec![&self.startup, &self.events];
        entries.extend(self.actions.iter());
        entries.extend(self.panes.iter());
        entries
    }
}

#[cfg(test)]
mod tests;
