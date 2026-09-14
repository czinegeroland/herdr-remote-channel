//! The Herdr plugin manifest (PRD section 13.2).
//!
//! Herdr discovers a plugin by reading `herdr-plugin.toml` from the default
//! branch of its repository, and installs one with
//! `herdr plugin install <owner>/<repo>`. The schema is the host's, not
//! ours: required `id`, `name`, `version`, and `min_herdr_version`, and
//! `[[build]]`, `[[startup]]`, `[[actions]]`, `[[events]]`, and `[[panes]]`
//! tables whose `command` is an argv array with no shell expansion.
//!
//! The file is generated here rather than hand-written and checked in on its
//! own, because the entry points the manifest advertises and the subcommands
//! the binary accepts have to agree. `crates/hrc-cli/tests/plugin_manifest.rs`
//! holds the checked-in file to this module's output, so a manifest edited by
//! hand and a manifest the code would produce cannot drift apart silently.
//!
//! Every command runs with the plugin directory as its working directory, so
//! `bin/hrc` is the copy the build step placed there. Resolving the binary
//! relative to the plugin root rather than through `PATH` means the plugin
//! works whether or not the install directory is on the `PATH` Herdr happens
//! to inherit.

use serde::Serialize;

/// Where the plugin's own copy of the binary lives, relative to the plugin
/// root that Herdr uses as the working directory.
///
/// No `.exe` suffix: `CreateProcess` appends one on Windows when the program
/// name carries no extension, and the build step writes `bin/hrc.exe` there.
pub const EXECUTABLE: &str = "bin/hrc";

/// The Herdr release whose documented manifest schema this file targets.
///
/// `contexts` on an action and `placement` on a pane are read from the 0.8.0
/// plugin reference. Declaring an older minimum would claim compatibility
/// with a schema this manifest has not been checked against.
pub const MIN_HERDR_VERSION: &str = "0.8.0";

/// A build step Herdr runs at install time, before it registers the plugin.
///
/// Build commands get the plugin root as their working directory and no
/// socket access. A failure halts the installation, which is the behavior we
/// want: a plugin registered without its binary would fail later and less
/// clearly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Build {
    /// The argv to run.
    pub command: Vec<String>,
    /// Platforms this step applies to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platforms: Option<Vec<String>>,
}

/// A command Herdr runs once per enabled plugin after session restoration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Startup {
    /// The argv to run.
    pub command: Vec<String>,
}

/// Something a person can invoke from the Herdr interface.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Action {
    /// Identifier, local to this plugin.
    pub id: String,
    /// What a person sees.
    pub title: String,
    /// Where the action is offered.
    pub contexts: Vec<String>,
    /// The argv to run.
    pub command: Vec<String>,
}

/// A host lifecycle event this plugin subscribes to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventHook {
    /// The host event name, such as `workspace.focused`.
    pub on: String,
    /// The argv to run.
    pub command: Vec<String>,
}

/// A pane this plugin draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PaneEntry {
    /// Identifier, local to this plugin.
    pub id: String,
    /// What a person sees.
    pub title: String,
    /// How Herdr places the pane.
    pub placement: String,
    /// The argv to run.
    pub command: Vec<String>,
}

/// The plugin manifest.
///
/// Field order is the order `herdr-plugin.toml` is written in, because
/// `toml` emits a struct's scalar fields before its tables and a reordered
/// struct would produce a file that no longer matches the checked-in one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Manifest {
    /// Plugin identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Plugin version.
    pub version: String,
    /// Oldest Herdr release this manifest is known to work against.
    pub min_herdr_version: String,
    /// One-line description.
    pub description: String,
    /// Platforms the plugin supports.
    pub platforms: Vec<String>,
    /// Install-time build steps.
    pub build: Vec<Build>,
    /// Run once when Herdr starts.
    pub startup: Vec<Startup>,
    /// Actions a person can trigger.
    pub actions: Vec<Action>,
    /// Host events this plugin reacts to.
    pub events: Vec<EventHook>,
    /// Panes this plugin draws.
    pub panes: Vec<PaneEntry>,
}

/// The argv for one `hrc herdr ...` entry point.
fn invoke(arguments: &[&str]) -> Vec<String> {
    let mut command = vec![EXECUTABLE.to_owned(), "herdr".to_owned()];
    command.extend(arguments.iter().map(|argument| (*argument).to_owned()));
    command
}

/// The manifest this build advertises.
pub fn manifest() -> Manifest {
    Manifest {
        id: "herdr-remote-channel".to_owned(),
        name: "Herdr Remote Channel".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        min_herdr_version: MIN_HERDR_VERSION.to_owned(),
        description: "Secure asynchronous communication between independent Herdr installations."
            .to_owned(),
        platforms: vec!["linux".to_owned(), "macos".to_owned(), "windows".to_owned()],
        build: vec![
            Build {
                command: vec!["sh".to_owned(), "herdr/build.sh".to_owned()],
                platforms: Some(vec!["linux".to_owned(), "macos".to_owned()]),
            },
            Build {
                command: vec![
                    "powershell".to_owned(),
                    "-NoProfile".to_owned(),
                    "-ExecutionPolicy".to_owned(),
                    "Bypass".to_owned(),
                    "-File".to_owned(),
                    "herdr/build.ps1".to_owned(),
                ],
                platforms: Some(vec!["windows".to_owned()]),
            },
        ],
        startup: vec![Startup {
            command: invoke(&["startup"]),
        }],
        actions: vec![Action {
            id: "inbox".to_owned(),
            title: "Remote channel inbox".to_owned(),
            contexts: vec!["workspace".to_owned(), "pane".to_owned()],
            command: invoke(&["action", "inbox"]),
        }],
        events: crate::event::HostEvent::ALL
            .into_iter()
            .map(|event| EventHook {
                on: event.as_str().to_owned(),
                command: invoke(&["event"]),
            })
            .collect(),
        panes: crate::pane::Pane::ALL
            .into_iter()
            .map(|pane| PaneEntry {
                id: pane.as_str().to_owned(),
                title: match pane {
                    crate::pane::Pane::Inbox => "Remote channel inbox".to_owned(),
                },
                // A split rather than an overlay: the inbox is something a
                // person watches while they work, not a modal they dismiss.
                placement: "split".to_owned(),
                command: invoke(&["pane", pane.as_str()]),
            })
            .collect(),
    }
}

impl Manifest {
    /// Every argv this manifest tells Herdr to run, build steps aside.
    ///
    /// Build steps are excluded deliberately: they run the platform's shell
    /// on a script, not the `hrc` binary, so holding them to the same rule
    /// would be holding them to the wrong one.
    pub fn commands(&self) -> Vec<&[String]> {
        let mut commands: Vec<&[String]> = Vec::new();
        commands.extend(self.startup.iter().map(|entry| entry.command.as_slice()));
        commands.extend(self.actions.iter().map(|entry| entry.command.as_slice()));
        commands.extend(self.events.iter().map(|entry| entry.command.as_slice()));
        commands.extend(self.panes.iter().map(|entry| entry.command.as_slice()));
        commands
    }

    /// The manifest as the `herdr-plugin.toml` Herdr reads.
    pub fn to_toml(&self) -> String {
        // Not `to_string_pretty`: that breaks every argv array across one line
        // per element, which turns a four-word command into six lines and
        // makes the file harder to read than the thing it describes.
        let body = toml::to_string(self).expect("the manifest is representable as TOML");

        format!("{HEADER}{body}")
    }
}

/// The comment block at the top of the generated file.
///
/// Carried here rather than added by whatever writes the file, so that the
/// bytes this module produces are the whole file and a test can compare them
/// to the checked-in one without knowing how it was written.
const HEADER: &str = "\
# Generated by `hrc herdr manifest`. Do not edit by hand.
#
# The source of truth is `crates/hrc-herdr/src/manifest.rs`, and
# `crates/hrc-cli/tests/plugin_manifest.rs` fails if this file and that
# module disagree. Regenerate with:
#
#     cargo run --quiet --bin hrc -- herdr manifest > herdr-plugin.toml

";

#[cfg(test)]
mod tests;
