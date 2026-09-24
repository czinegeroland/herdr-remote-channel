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

/// The npm package that carries the executable.
pub const PACKAGE: &str = "herdr-remote-channel";

/// The plugin identifier Herdr registers this plugin under.
///
/// Named here rather than written inline in the manifest, because the inbox
/// pane has to name the plugin back to Herdr when it asks for the review
/// popup. Two spellings of one id would mean an inbox that silently fails to
/// open anything.
pub const PLUGIN_ID: &str = "herdr-remote-channel";

/// The environment variable that carries the message the review popup opens.
///
/// Herdr's `plugin pane open --env KEY=VALUE` sets it on the launched
/// process, so the value reaches exactly that popup and no other pane, no
/// other workspace, and nothing that reads a file. It carries an identifier,
/// never a body: a body in an environment variable would be readable from
/// the process table of the machine, which is the opposite of quarantine.
pub const REVIEW_TARGET_ENV: &str = "HRC_REVIEW_MESSAGE";

/// The environment variable that carries the message whose thread the
/// thread pane opens on. Same rules as [`REVIEW_TARGET_ENV`]: an identifier,
/// never content, set on that one process.
pub const THREAD_TARGET_ENV: &str = "HRC_THREAD_MESSAGE";

/// The GitHub repository the agent skill is installed from.
///
/// `skills add` takes `owner/repo` and has no flag for a tag or a commit, so
/// the skill always comes from the default branch while the executable is
/// pinned to this build. That asymmetry is deliberate but real: a skill
/// describing commands a pinned older binary does not have would send an
/// agent at a CLI that rejects it, so the skill's own contract tests hold it
/// to the CLI in the same repository (decision DEC-079).
pub const REPOSITORY: &str = "czinegeroland/herdr-remote-channel";

/// The skill installed alongside the executable.
pub const SKILL: &str = "herdr-remote-channel";

/// The launcher the build step installs, relative to the plugin root that
/// Herdr uses as the working directory.
///
/// Entry points run it as `node <this>` rather than executing it directly.
/// `node` is `node.exe` on Windows, so it resolves however Herdr spawns a
/// command; `node_modules/.bin/hrc` would be `hrc.cmd` there, which a bare
/// `CreateProcess` does not find. The launcher itself selects the platform
/// package npm installed and execs the real binary, passing arguments and
/// the exit code straight through.
pub const LAUNCHER: &str = "node_modules/herdr-remote-channel/bin.js";

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
    /// Popup width, in terminal cells or as a percentage. Popups only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<String>,
    /// Popup height, in terminal cells or as a percentage. Popups only.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<String>,
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
    /// Terminal URLs whose modified click this plugin claims.
    pub link_handlers: Vec<LinkHandler>,
}

/// A terminal URL pattern routed to one of this plugin's actions.
///
/// Herdr sends a modified click on a matching URL to `action` instead of to
/// the browser, and hands it the clicked URL. The pattern is therefore a
/// claim on a gesture that belongs to the person, not to this plugin, which
/// is why it matches only what an invitation prints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LinkHandler {
    /// Identifier, local to this plugin.
    pub id: String,
    /// What a person sees.
    pub title: String,
    /// The regular expression Herdr matches a clicked URL against.
    pub pattern: String,
    /// The action to invoke.
    pub action: String,
}

/// The argv that installs this version's executable from npm.
///
/// `prefix` wraps the command for hosts that spawn a process directly rather
/// than through a shell: on Windows `npm` is `npm.cmd`, which `CreateProcess`
/// will not find, so the step goes through `cmd /c`. `cmd.exe` resolves under
/// either spawning model, which is why this is written once with a wrapper
/// instead of twice.
///
/// The version is pinned to this build's own, so the manifest Herdr read and
/// the executable it installs are the same release rather than whatever
/// `latest` happens to be at install time.
///
/// `--no-save` because the plugin root is not a package: the install needs
/// `node_modules`, not a `package.json` describing it.
fn npm_install(prefix: &[&str]) -> Vec<String> {
    let mut command: Vec<String> = prefix
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
    command.extend(
        ["npm", "install", "--no-save", "--no-audit", "--no-fund"]
            .into_iter()
            .map(str::to_owned),
    );
    command.push(format!("{PACKAGE}@{}", env!("CARGO_PKG_VERSION")));
    command
}

/// The argv that installs the agent skill.
///
/// Installing the plugin without it leaves the panes reachable and the agent
/// unable to drive the CLI, which is most of the way to the point rather than
/// at it. `--agent claude-code` rather than `*`: a plugin install should not
/// write a skill into every agent on the machine it can find. `--global` so
/// it is there regardless of which directory Herdr opens, and `--yes` because
/// a build step has no one to answer a prompt.
///
/// `prefix` wraps it for the same reason `npm_install` does: `npx` is
/// `npx.cmd` on Windows, which `CreateProcess` will not find.
fn skill_install(prefix: &[&str]) -> Vec<String> {
    let mut command: Vec<String> = prefix
        .iter()
        .map(|argument| (*argument).to_owned())
        .collect();
    command.extend(
        ["npx", "--yes", "skills", "add", REPOSITORY]
            .into_iter()
            .map(str::to_owned),
    );
    command.extend(
        [
            "--skill",
            SKILL,
            "--agent",
            "claude-code",
            "--global",
            "--yes",
        ]
        .into_iter()
        .map(str::to_owned),
    );
    command
}

/// The argv that asks Herdr to open the review popup on one message.
///
/// Run against `HERDR_BIN_PATH`, which Herdr injects into every plugin
/// command and which resolves the same way on a Unix socket and a Windows
/// named pipe. This is the documented host API for opening a registered pane
/// with a local-only argument (decision DEC-086); the `github-link-preview`
/// example plugin routes a clicked URL into a split the same way.
///
/// `message_id` must already be a well-formed ULID. The caller checks, and
/// opens the unfocused pending list when it is not, so the only thing this
/// puts on a host command line is 26 characters of Crockford base32
/// (decision DEC-087).
pub fn open_review(message_id: &str) -> Vec<String> {
    [
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        crate::pane::Pane::Review.as_str(),
        "--placement",
        "popup",
        "--focus",
        "--env",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(format!("{REVIEW_TARGET_ENV}={message_id}")))
    .collect()
}

/// The argv that asks Herdr to open the thread pane on one message's thread.
///
/// `message_id` must already be a well-formed ULID, for the same reason as
/// [`open_review`]: it is the only thing this puts on a host command line.
pub fn open_thread(message_id: &str) -> Vec<String> {
    [
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        crate::pane::Pane::Thread.as_str(),
        "--placement",
        "zoomed",
        "--focus",
        "--env",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain(std::iter::once(format!("{THREAD_TARGET_ENV}={message_id}")))
    .collect()
}

/// The argv that asks Herdr to open the review popup with nothing selected.
///
/// The fallback when a row's identifier cannot be handed to the host. A
/// person still reaches every pending message from it, which is the failure
/// this code wants: an unfocused list rather than the wrong body.
pub fn open_review_list() -> Vec<String> {
    [
        "plugin",
        "pane",
        "open",
        "--plugin",
        PLUGIN_ID,
        "--entrypoint",
        crate::pane::Pane::Review.as_str(),
        "--placement",
        "popup",
        "--focus",
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// The argv for one `hrc herdr ...` entry point.
fn invoke(arguments: &[&str]) -> Vec<String> {
    let mut command = vec!["node".to_owned(), LAUNCHER.to_owned(), "herdr".to_owned()];
    command.extend(arguments.iter().map(|argument| (*argument).to_owned()));
    command
}

/// What a person sees for one pane, used for both its pane entry and its
/// action so the two cannot drift into different names for one screen.
fn pane_title(pane: crate::pane::Pane) -> String {
    match pane {
        crate::pane::Pane::Inbox => "Remote channel inbox".to_owned(),
        crate::pane::Pane::Setup => "Remote channel setup".to_owned(),
        crate::pane::Pane::Compose => "Remote channel compose".to_owned(),
        crate::pane::Pane::Context => "Remote channel context".to_owned(),
        crate::pane::Pane::Joins => "Remote channel join requests".to_owned(),
        crate::pane::Pane::Members => "Remote channel members".to_owned(),
        crate::pane::Pane::Review => "Remote channel review".to_owned(),
        crate::pane::Pane::Thread => "Remote channel thread".to_owned(),
    }
}

/// The manifest this build advertises.
pub fn manifest() -> Manifest {
    Manifest {
        id: PLUGIN_ID.to_owned(),
        name: "Herdr Remote Channel".to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
        min_herdr_version: MIN_HERDR_VERSION.to_owned(),
        description: "Secure asynchronous communication between independent Herdr installations."
            .to_owned(),
        platforms: vec!["linux".to_owned(), "macos".to_owned(), "windows".to_owned()],
        // Installing this plugin needs no compiler. The executable is
        // published to npm, one package per platform carrying its own binary,
        // and npm serves the bytes under its own integrity hash; every
        // archive was checked against its published SHA-256 before it was
        // packed. The install is a download and takes about a second.
        //
        // It used to be three `cargo` commands, and that was a mistake worth
        // recording rather than quietly deleting. This project had already
        // built the npm channel precisely so that no toolchain would be
        // needed, and then left the one install path most people take
        // building from source anyway. The two decisions contradicted each
        // other. It failed on the first real Windows install with
        // `linker `link.exe` not found`: the MSVC target needs Visual Studio
        // Build Tools, which is a multi-gigabyte prerequisite to read an
        // inbox pane. A source build belongs in the development flow
        // (`herdr plugin link`), not in `herdr plugin install`.
        build: vec![
            Build {
                command: npm_install(&["cmd", "/c"]),
                platforms: Some(vec!["windows".to_owned()]),
            },
            Build {
                command: npm_install(&[]),
                platforms: Some(vec!["linux".to_owned(), "macos".to_owned()]),
            },
            Build {
                command: skill_install(&["cmd", "/c"]),
                platforms: Some(vec!["windows".to_owned()]),
            },
            Build {
                command: skill_install(&[]),
                platforms: Some(vec!["linux".to_owned(), "macos".to_owned()]),
            },
        ],
        startup: vec![Startup {
            command: invoke(&["startup"]),
        }],
        // One action per pane. Panes are opened by the host; actions are what
        // a person can reach for. A pane with no action is a screen that
        // exists and cannot be asked for, which is how this plugin shipped an
        // approval interface nobody could open.
        actions: crate::pane::Pane::ALL
            .into_iter()
            .map(|pane| Action {
                id: pane.as_str().to_owned(),
                title: pane_title(pane),
                contexts: vec!["workspace".to_owned(), "pane".to_owned()],
                command: invoke(&["action", pane.as_str()]),
            })
            .collect(),
        events: crate::event::HostEvent::ALL
            .into_iter()
            .map(|event| EventHook {
                on: event.as_str().to_owned(),
                command: invoke(&["event"]),
            })
            .collect(),
        // One handler, matching only what `hrc invite create` prints. A
        // pattern that matched bare repository URLs would take over
        // modified clicks on every GitHub link in every pane, which is a
        // gesture this plugin does not own.
        link_handlers: vec![LinkHandler {
            id: "join".to_owned(),
            title: "Join this remote channel".to_owned(),
            pattern: crate::link::JOIN_PATTERN.to_owned(),
            action: crate::pane::Pane::Setup.as_str().to_owned(),
        }],
        panes: crate::pane::Pane::ALL
            .into_iter()
            .map(|pane| PaneEntry {
                id: pane.as_str().to_owned(),
                title: pane_title(pane),
                placement: match pane {
                    // A split rather than an overlay: the inbox is something
                    // a person watches while they work, not a modal they
                    // dismiss.
                    crate::pane::Pane::Inbox => "split".to_owned(),
                    // The approval screen is modal on purpose. It is the one
                    // surface where a quarantined body is on display, and a
                    // popup is session-modal in Herdr: it takes every key,
                    // Escape included, and it is not part of the tiled
                    // layout, so a revealed body cannot be left sitting in a
                    // corner of the workspace while the human does something
                    // else. Deciding is meant to be the thing they are doing.
                    // Admitting a member is the same kind of decision as
                    // approving content, and gets the same modal treatment.
                    crate::pane::Pane::Setup
                    | crate::pane::Pane::Compose
                    | crate::pane::Pane::Context
                    | crate::pane::Pane::Joins
                    | crate::pane::Pane::Members
                    | crate::pane::Pane::Review => "popup".to_owned(),
                    // A conversation is the one thing that wants the whole
                    // screen and wants to stay open while someone reads it.
                    crate::pane::Pane::Thread => "zoomed".to_owned(),
                },
                // Sized only where the host reads it. A width on a split
                // pane is not a smaller split, it is a field the manifest
                // schema does not define there.
                width: match pane {
                    crate::pane::Pane::Inbox | crate::pane::Pane::Thread => None,
                    crate::pane::Pane::Setup
                    | crate::pane::Pane::Compose
                    | crate::pane::Pane::Context
                    | crate::pane::Pane::Joins
                    | crate::pane::Pane::Members
                    | crate::pane::Pane::Review => Some("80%".to_owned()),
                },
                height: match pane {
                    crate::pane::Pane::Inbox | crate::pane::Pane::Thread => None,
                    crate::pane::Pane::Setup
                    | crate::pane::Pane::Compose
                    | crate::pane::Pane::Context
                    | crate::pane::Pane::Joins
                    | crate::pane::Pane::Members
                    | crate::pane::Pane::Review => Some("80%".to_owned()),
                },
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
