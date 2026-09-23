//! The Herdr plugin entry points: startup, actions, events and panes.

use super::*;

/// `hrc herdr startup`: what Herdr runs when it loads the plugin.
///
/// Returns the manifest this build advertises together with the current
/// sidebar line, so a host that caches the manifest and a host that re-reads
/// it on every launch both get a consistent answer.
///
/// It deliberately does not start the daemon. `hrc daemon` is a resident
/// process with its own lifecycle, and a plugin hook that spawned one would
/// give the daemon the plugin's lifetime — which is the arrangement PRD
/// section 29 lists as a known limitation to avoid.
pub fn herdr_startup(context: &Context) -> Result<Value> {
    let manifest = hrc_herdr::manifest();
    let sidebar = herdr_sidebar(context)?;

    Ok(json!({
        "status": "ok",
        "manifest": manifest,
        "sidebar": sidebar.render(),
        "inbox": place_inbox(context),
    }))
}

/// Puts the inbox on screen when Herdr starts (PRD section 23.2).
///
/// Without this the startup hook computed a sidebar line, returned JSON, and
/// exited — so a person who opened Herdr saw nothing, and the side view they
/// were meant to keep open existed only if they went and ran a command for it
/// every session. That is the fifth time this project has shipped working
/// code reachable from nothing, and the first one a user found rather than a
/// test (decision DEC-094).
///
/// Three things stop it being intrusive. It opens without focus, because a
/// split that grabs the keyboard while someone is typing is the behaviour
/// that gets a plugin uninstalled. It opens nothing when no channel is
/// configured, because a pane whose only content is an error is worse than no
/// pane. And it records the pane it opened, so the live handoff that re-runs
/// startup hooks while keeping panes alive does not leave two inboxes side by
/// side.
///
/// Every failure is reported rather than raised. A failed startup hook shows
/// a person a broken plugin, and not placing a pane is not a broken plugin.
pub(super) fn place_inbox(context: &Context) -> Value {
    let (config, problems) = plugin_config();
    let mut answer = placement(context, &config);

    // Attached to whatever the placement decided rather than repeated on
    // each of its six exits, because a setting that was ignored has to be
    // reported on every one of them and the exit that forgot would be the
    // one somebody hit. The hook's answer is what Herdr's plugin log
    // records, which is where a person looks when a setting seems not to
    // work.
    if let Some(object) = answer.as_object_mut() {
        object.insert(
            "ignored".into(),
            Value::from(
                problems
                    .iter()
                    .map(hrc_herdr::ConfigProblem::as_str)
                    .collect::<Vec<_>>(),
            ),
        );
    }

    answer
}

/// Where the inbox goes, given what this installation was configured to do.
pub(super) fn placement(context: &Context, config: &hrc_herdr::Config) -> Value {
    if !config.open_inbox_at_startup {
        return json!({
            "opened": false,
            "reason": "`inbox.open_at_startup` is off in this installation's configuration",
        });
    }

    let database = match Database::open(context.paths.database()) {
        Ok(database) => database,
        Err(error) => {
            return json!({ "opened": false, "reason": error.to_string() });
        }
    };

    match database.channels() {
        Ok(channels) if channels.is_empty() => {
            return json!({
                "opened": false,
                "reason": "no channel is configured yet; `Remote channel setup` is where that starts",
            });
        }
        Ok(_) => {}
        Err(error) => return json!({ "opened": false, "reason": error.to_string() }),
    }

    let mut host = match crate::herdr_host::Host::connect() {
        Ok(host) => host,
        Err(error) => return json!({ "opened": false, "reason": error.to_string() }),
    };

    if let Some(pane_id) = remembered_inbox_pane()
        && host.pane_is_open(&pane_id)
    {
        return json!({ "opened": false, "reason": "already open", "pane": pane_id });
    }

    match host.open_inbox() {
        Ok(pane_id) => {
            remember_inbox_pane(&pane_id);

            // Herdr splits evenly, which is too much window for a list of
            // names and ages. Reported rather than raised: a pane that stayed
            // the size the host chose is still a working pane.
            let narrowed = host.narrow(&pane_id, config.inbox_share);

            json!({
                "opened": true,
                "pane": pane_id,
                "narrowed": narrowed.is_ok(),
                "share": config.inbox_share,
            })
        }
        Err(error) => json!({ "opened": false, "reason": error.to_string()  }),
    }
}

/// Reads this installation's plugin configuration.
///
/// Herdr names the directory; a missing variable means the plugin is not
/// running under Herdr at all, which is the ordinary case for a test and for
/// someone driving `hrc` directly, and the defaults are correct there.
///
/// A file that cannot be read is reported rather than raised. Configuration
/// governs placement and volume, never a gate, so nothing here is worth
/// failing a startup hook over — and a hook that failed would show a person
/// a broken plugin for a stray character in a file they wrote.
pub(crate) fn plugin_config() -> (hrc_herdr::Config, Vec<hrc_herdr::ConfigProblem>) {
    let Some(directory) = std::env::var_os(hrc_herdr::config::CONFIG_ENV) else {
        return (hrc_herdr::Config::default(), Vec::new());
    };

    let path = std::path::PathBuf::from(directory).join(hrc_herdr::config::CONFIG_FILE);

    match std::fs::read_to_string(&path) {
        Ok(text) => hrc_herdr::config::parse(&text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            (hrc_herdr::Config::default(), Vec::new())
        }
        Err(_) => (
            hrc_herdr::Config::default(),
            vec![hrc_herdr::ConfigProblem::Unreadable],
        ),
    }
}

/// Where the identifier of the inbox pane is kept between startups.
///
/// `HERDR_PLUGIN_STATE_DIR` is the directory Herdr creates for exactly this
/// and never looks inside. A pane identifier is local topology and is the
/// only thing written there.
pub(super) fn inbox_pane_record() -> Option<std::path::PathBuf> {
    let directory = std::env::var(hrc_herdr::host::STATE_ENV).ok()?;
    (!directory.is_empty()).then(|| std::path::Path::new(&directory).join("inbox-pane"))
}

/// The pane this plugin last placed the inbox in, if it recorded one.
pub(super) fn remembered_inbox_pane() -> Option<String> {
    let recorded = std::fs::read_to_string(inbox_pane_record()?).ok()?;
    let recorded = recorded.trim();

    (!recorded.is_empty()).then(|| recorded.to_owned())
}

/// Records the pane the inbox was placed in.
///
/// A write that fails is ignored: the cost is a second inbox after a Herdr
/// handoff, and failing the startup hook over it would cost the whole plugin.
pub(super) fn remember_inbox_pane(pane_id: &str) {
    if let Some(path) = inbox_pane_record() {
        let _ = std::fs::write(path, pane_id);
    }
}

/// `hrc herdr action <name>`: run a named action from the manifest.
pub fn herdr_action(context: &Context, action: &str) -> Result<Value> {
    // Actions and panes are named separately in the manifest but the inbox
    // action and the inbox pane show the same thing, so they share a body.
    // Every action opens the pane of the same name. They are declared
    // separately in the manifest because the host offers them differently —
    // a pane is placed, an action is invoked — but they show the same screen,
    // so resolving through the pane parser keeps one list of what exists.
    match hrc_herdr::Pane::parse(action) {
        Some(pane) => herdr_pane(context, pane.as_str()),
        None => Err(CliError::UnknownHerdrTarget {
            kind: "action",
            name: action.to_owned(),
        }),
    }
}

/// `hrc herdr event`: handle one event Herdr wrote to standard input.
///
/// The reaction is returned rather than acted on. A plugin hook is a
/// short-lived process; refreshing a pane is the host's job once it knows
/// something changed, and the alternative — a hook that reaches into the
/// daemon on every tick — is the repeated spawning PRD section 13.2 says to
/// avoid.
pub fn herdr_event(context: &Context, event: Option<&str>) -> Result<Value> {
    // An unrecognized event name is ignored rather than refused. Herdr names
    // the event in an environment variable and may deliver one this plugin
    // did not subscribe to; exiting non-zero would show a person a failed
    // plugin for something that is not a failure.
    let reaction = hrc_herdr::reaction_to(event);

    // The sidebar is cheap and is what every refresh reaction needs, so it
    // travels with the answer rather than costing the host a second process.
    let sidebar = match reaction {
        hrc_herdr::Reaction::Ignore => None,
        _ => Some(herdr_sidebar(context)?.render()),
    };

    Ok(json!({
        "status": "ok",
        "event": event,
        "reaction": reaction,
        "sidebar": sidebar,
    }))
}

/// `hrc herdr manifest`: the `herdr-plugin.toml` this build advertises.
///
/// Printed rather than written, so regenerating the checked-in file is an
/// explicit redirection a person performs and reviews, not something a
/// command does to a working tree on its own.
pub fn herdr_manifest() -> Result<Value> {
    Ok(json!({
        "status": "ok",
        "manifest": hrc_herdr::manifest(),
        "toml": hrc_herdr::manifest().to_toml(),
    }))
}

/// `hrc herdr pane <name>`: render one pane.
pub fn herdr_pane(context: &Context, pane: &str) -> Result<Value> {
    let pane = hrc_herdr::Pane::parse(pane).ok_or_else(|| CliError::UnknownHerdrTarget {
        kind: "pane",
        name: pane.to_owned(),
    })?;

    match pane {
        // The trusted approval screen. Herdr opens this as a session-modal
        // popup, which is a real terminal, so the screen runs here rather
        // than rendering rows for the host to print.
        //
        // The inbox side view asks Herdr for this popup with the selected
        // message in an environment variable, which is how a registered pane
        // takes a local argument (decision DEC-086). Without one the screen
        // lists everything pending, which is what opening the pane directly
        // does.
        hrc_herdr::Pane::Review => review::review(
            context,
            review::review_target().as_deref(),
            &review::local_agent(),
        ),

        // Membership approval, the other decision a human owns.
        hrc_herdr::Pane::Joins => review::review_joins(context),

        // Removing a member and revoking a device, the other half of
        // membership that section 22.7 puts behind the boundary.
        hrc_herdr::Pane::Members => review::members(context),

        // Writing a message, so that saying something needs no other tool.
        hrc_herdr::Pane::Compose => review::compose(context),

        // Disclosing a context package, which section 22.4 puts behind the
        // same boundary as reading a quarantined body.
        hrc_herdr::Pane::Context => review::context(context),

        // Getting a channel in the first place.
        hrc_herdr::Pane::Setup => review::setup(context),

        // The inbox. Interactive when a person is at the terminal, and the
        // same rows as JSON when something else is reading them.
        hrc_herdr::Pane::Inbox => review::inbox(context),
    }
}

/// The sidebar line for every configured channel (PRD section 23.1).
pub(super) fn herdr_sidebar(context: &Context) -> Result<hrc_herdr::Sidebar> {
    herdr_sidebar_for(&Database::open(context.paths.database())?)
}

/// The same, over a database the caller already has open.
///
/// The inbox side view reloads once a second and holds its own handle;
/// opening a second one per tick to read a counter would be a file open per
/// second for the life of the pane.
pub(crate) fn herdr_sidebar_for(database: &Database) -> Result<hrc_herdr::Sidebar> {
    let mut statuses = Vec::new();
    let mut unread = 0usize;
    let mut unanswered = 0usize;
    let mut awaiting_answer = 0usize;

    for channel in database.channels()? {
        let counts = database.channel_counts(&channel.channel_id)?;
        unread = unread.saturating_add(counts.unread as usize);

        let outstanding = database.unanswered(&channel.channel_id)?;
        unanswered = unanswered.saturating_add(outstanding.owed);
        awaiting_answer = awaiting_answer.saturating_add(outstanding.awaiting);

        statuses.push(ChannelStatus {
            local_name: channel.local_name,
            roster_epoch: channel.roster_epoch,
            pending: counts.pending_approval as usize,
            halted: channel.halted_reason.is_some(),
        });
    }

    // The age of the last fetch is not recorded as a timestamp, only as a
    // transport revision, so the sidebar reports what it actually knows
    // rather than inventing a duration. Wiring a real age is the work
    // `HRC-SYNC-003` evidence will have to cite when it lands.
    Ok(hrc_herdr::Sidebar::from_status(
        &statuses,
        unread,
        None,
        unanswered,
        awaiting_answer,
    ))
}
