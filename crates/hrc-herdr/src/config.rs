//! User configuration for the plugin (`$HERDR_PLUGIN_CONFIG_DIR/config.json`).
//!
//! Herdr gives every plugin a configuration directory and a state directory,
//! and until this module existed this plugin used neither for configuration:
//! nothing it did was adjustable. That is most visible in the startup hook,
//! which opens a pane in someone's workspace without being asked and with no
//! way to say no — the behaviour a configuration file exists to make
//! optional.
//!
//! Three rules shape what follows.
//!
//! A missing file is not an error. Most installations will never have one,
//! and a startup hook that failed because a person had not written
//! configuration would be worse than one that opened an unwanted pane.
//!
//! A malformed file is also not a failure, but it is never silent. The
//! defaults are used and the reason is returned to the caller, which puts it
//! in the startup hook's answer and so in Herdr's plugin log. Silently
//! ignoring a file somebody wrote is how a setting appears not to work.
//!
//! Nothing here is security-relevant, and nothing here may become so. This
//! file is ordinary user-editable text with no signature and no integrity
//! check: anything able to write it is already able to write the plugin's
//! own state. So it configures placement and volume, never whether a gate
//! applies, who may decide, or what a surface may show.

use serde::Deserialize;

/// The environment variable Herdr uses to name the configuration directory.
pub const CONFIG_ENV: &str = "HERDR_PLUGIN_CONFIG_DIR";

/// The file read from that directory.
pub const CONFIG_FILE: &str = "config.json";

/// The narrowest share of a split the inbox may be asked to take.
///
/// Below this the pane cannot draw the two fields section 23.2 says never
/// go — who it is from and whether it needs you — at any terminal width
/// worth having, so a smaller number is refused rather than honoured into
/// an unreadable pane.
pub const MIN_SHARE: f64 = 0.1;

/// The widest share the inbox may be asked to take.
///
/// Past this it is no longer a side view. Somebody who wants the inbox to
/// be the whole screen wants the `Remote channel inbox` action, not a
/// permanently enormous split.
pub const MAX_SHARE: f64 = 0.9;

/// What this installation has been told to do.
#[derive(Debug, Clone, PartialEq)]
pub struct Config {
    /// Whether the startup hook places the inbox split.
    pub open_inbox_at_startup: bool,
    /// The share of its split the inbox takes.
    pub inbox_share: f64,
    /// Whether notifications are raised at all.
    pub notifications: bool,
    /// Whether a count is reported beside the inbox pane in Herdr's sidebar.
    pub pane_token: bool,
    /// Whether a count is written to the terminal window title.
    pub window_title: bool,
    /// Whether an approved delivery to a working agent waits until it is
    /// idle rather than landing mid-turn.
    pub wait_for_idle: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            open_inbox_at_startup: true,
            inbox_share: crate::host::INBOX_SHARE,
            notifications: true,
            pane_token: true,
            // Off unless somebody asks for it. The window title belongs to
            // the client, not to this plugin: anything else that sets it
            // will be overwritten, and a plugin that quietly took over a
            // surface it does not own would deserve to be uninstalled.
            window_title: false,
            wait_for_idle: true,
        }
    }
}

/// Why a configuration file was not used.
///
/// Carried rather than logged, so the caller decides where it surfaces. The
/// startup hook puts it in its answer, which is what Herdr records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigProblem {
    /// The file exists but could not be read.
    Unreadable,
    /// The file is not JSON, or not the shape this build understands.
    Malformed,
    /// A value was outside the range this build will honour.
    ///
    /// Named rather than counted: a person who typed `0.02` needs to know
    /// which setting was ignored, not that one was.
    OutOfRange {
        /// The setting that was refused.
        setting: &'static str,
    },
    /// A key this build does not understand.
    ///
    /// Reported rather than refused. A future version may add settings, and
    /// an older build failing on one would make configuration unupgradable;
    /// but a typo that silently does nothing is the thing that wastes an
    /// afternoon.
    Unknown {
        /// The key that was not recognized.
        key: String,
    },
}

impl ConfigProblem {
    /// Fixed local wording for the problem.
    pub fn as_str(&self) -> String {
        match self {
            ConfigProblem::Unreadable => "the configuration file could not be read".into(),
            ConfigProblem::Malformed => "the configuration file is not valid JSON".into(),
            ConfigProblem::OutOfRange { setting } => {
                format!("`{setting}` is outside the range this build honours, so it was ignored")
            }
            ConfigProblem::Unknown { key } => {
                format!("`{key}` is not a setting this build understands, so it was ignored")
            }
        }
    }
}

/// The document as written, before it is checked.
///
/// Every field optional, because a file that sets one thing should not have
/// to restate the rest. `deny_unknown_fields` is deliberately *not* used:
/// an unrecognized key is reported by [`parse`] and the rest of the file is
/// still honoured, rather than one typo discarding every setting beside it.
#[derive(Debug, Default, Deserialize)]
struct Document {
    #[serde(default)]
    inbox: Option<InboxSection>,
    #[serde(default)]
    notifications: Option<NotificationSection>,
    #[serde(default)]
    indicator: Option<IndicatorSection>,
    #[serde(default)]
    delivery: Option<DeliverySection>,
}

#[derive(Debug, Default, Deserialize)]
struct DeliverySection {
    #[serde(default)]
    wait_for_idle: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct InboxSection {
    #[serde(default)]
    open_at_startup: Option<bool>,
    #[serde(default)]
    share: Option<f64>,
}

#[derive(Debug, Default, Deserialize)]
struct NotificationSection {
    #[serde(default)]
    enabled: Option<bool>,
}

#[derive(Debug, Default, Deserialize)]
struct IndicatorSection {
    #[serde(default)]
    pane_token: Option<bool>,
    #[serde(default)]
    window_title: Option<bool>,
}

/// Every key this build understands, as a path.
const KNOWN: &[&str] = &[
    "inbox",
    "inbox.open_at_startup",
    "inbox.share",
    "notifications",
    "notifications.enabled",
    "indicator",
    "indicator.pane_token",
    "indicator.window_title",
    "delivery",
    "delivery.wait_for_idle",
];

/// Reads a configuration document and reports what could not be used.
///
/// Always returns a usable [`Config`]. Problems accumulate rather than
/// stopping at the first: a person fixing their file wants the whole list,
/// not one item per attempt.
pub fn parse(text: &str) -> (Config, Vec<ConfigProblem>) {
    let mut problems = Vec::new();
    let mut config = Config::default();

    let Ok(raw) = serde_json::from_str::<serde_json::Value>(text) else {
        return (config, vec![ConfigProblem::Malformed]);
    };

    problems.extend(unknown_keys(&raw));

    let Ok(document) = serde_json::from_value::<Document>(raw) else {
        // Well-formed JSON of the wrong shape: a string where a number
        // belongs, say. The keys were still worth reporting above.
        problems.push(ConfigProblem::Malformed);
        return (config, problems);
    };

    if let Some(inbox) = document.inbox {
        if let Some(open) = inbox.open_at_startup {
            config.open_inbox_at_startup = open;
        }

        if let Some(share) = inbox.share {
            if (MIN_SHARE..=MAX_SHARE).contains(&share) {
                config.inbox_share = share;
            } else {
                problems.push(ConfigProblem::OutOfRange {
                    setting: "inbox.share",
                });
            }
        }
    }

    if let Some(notifications) = document.notifications
        && let Some(enabled) = notifications.enabled
    {
        config.notifications = enabled;
    }

    if let Some(indicator) = document.indicator {
        if let Some(pane_token) = indicator.pane_token {
            config.pane_token = pane_token;
        }
        if let Some(window_title) = indicator.window_title {
            config.window_title = window_title;
        }
    }

    if let Some(delivery) = document.delivery
        && let Some(wait) = delivery.wait_for_idle
    {
        config.wait_for_idle = wait;
    }

    (config, problems)
}

/// Keys in the document that this build does not understand.
fn unknown_keys(raw: &serde_json::Value) -> Vec<ConfigProblem> {
    let Some(sections) = raw.as_object() else {
        return Vec::new();
    };

    let mut unknown = Vec::new();
    for (section, value) in sections {
        if !KNOWN.contains(&section.as_str()) {
            unknown.push(ConfigProblem::Unknown {
                key: section.clone(),
            });
            continue;
        }

        let Some(settings) = value.as_object() else {
            continue;
        };

        for setting in settings.keys() {
            let path = format!("{section}.{setting}");
            if !KNOWN.contains(&path.as_str()) {
                unknown.push(ConfigProblem::Unknown { key: path });
            }
        }
    }

    unknown
}

#[cfg(test)]
mod tests;
