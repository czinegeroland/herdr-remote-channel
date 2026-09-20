//! The trusted approval screen, and the only path that opens it.
//!
//! PRD section 19.4 says a quarantined body must be shown in a trusted
//! interface rather than returned to the agent asking for approval, and
//! section 22.7 says direct *non-interactive* invocation must fail. The word
//! doing the work there is "non-interactive": the boundary is not that a
//! human may never approve from a terminal, it is that a program may not.
//!
//! So this refuses unless standard input and standard output are both a
//! terminal. A pipe, a captured subprocess, or an agent's tool call is not a
//! human, and each of those is exactly what the boundary exists to stop.
//! `--json` is refused earlier still, before dispatch.
//!
//! The screen itself decides nothing. It reports what the human chose and
//! the daemon's trusted interface applies it, so the approval rules live in
//! one place rather than two.

use std::time::Duration;

use hrc_core::rpc::{TrustedRequest, WireDecision};
use hrc_ipc::Client;
use hrc_ipc::endpoint::{Endpoint, Interface};
use hrc_storage::Database;
use hrc_tui::{
    App, ComposeApp, ComposeOutcome, ContextApp, ContextOutcome, JoinApp, JoinOutcome,
    MemberOutcome, MembersApp, Outcome, PassphraseApp, PassphraseOutcome, PendingItem, PendingJoin,
    Recipient, SetupApp, SetupOutcome,
};
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::commands::Context;
use crate::error::{CliError, Result};

/// How long an approval authorization stays usable.
///
/// Short because it is consumed inside the same call that issues it; this is
/// a bound on the clock skew between issuing and consuming, not a window in
/// which a human is expected to act.
const AUTHORIZATION_LIFETIME_SECONDS: i64 = 300;

/// How long to wait for the daemon's trusted endpoint to answer.
const CONNECT_ATTEMPTS: u32 = 40;

/// Gap between connection attempts.
const CONNECT_INTERVAL: Duration = Duration::from_millis(25);

/// Opens the trusted approval screen.
///
/// `message_id` narrows the screen to one message. Without it every pending
/// message is listed, which is what the Herdr pane opens.
pub fn review(context: &Context, message_id: Option<&str>, agent: &str) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;

    let pending = runtime.block_on(collect_pending(context, &endpoint, message_id))?;
    if pending.is_empty() {
        return Ok(json!({
            "status": "ok",
            "pending": 0,
            "decided": Value::Null,
        }));
    }

    // Enumerated here, so the screen offers what Herdr has right now rather
    // than a name this process guessed from its own environment. When Herdr
    // is not reachable the list is empty and the screen says so: keeping and
    // declining still work, and delivering refuses instead of failing after
    // the person has confirmed it.
    let destinations = destinations(agent);
    let bodies: std::collections::HashMap<String, String> = pending
        .iter()
        .map(|item| (item.message_id.clone(), item.body.clone()))
        .collect();

    let mut app = App::new(pending, destinations);
    let outcome = hrc_tui::run(&mut app).map_err(|source| CliError::Io {
        action: "run the trusted approval screen",
        source,
    })?;

    let delivery = match &outcome {
        Outcome::DeliverToAgent {
            message_id, target, ..
        } => Some((message_id.clone(), target.clone())),
        _ => None,
    };

    // Re-checked after the screen closes, because the list it offered was
    // read when the screen opened and a person may have spent minutes
    // reading. A pane closed in between would otherwise be approved into:
    // the daemon would record a delivery, the hand-over would fail, and the
    // message would sit approved with nothing holding it. Nothing has been
    // recorded yet at this point, so refusing here leaves it pending —
    // which is the state a person can act on again.
    if let Some((_, target)) = &delivery
        && let Some(gone) = departed(target)
    {
        return Err(gone);
    }

    let Some((message_id, decision, action)) = wire_decision(outcome) else {
        return Ok(json!({
            "status": "ok",
            "decided": Value::Null,
        }));
    };

    let applied = runtime.block_on(apply(&endpoint, &message_id, decision))?;

    // Only after the daemon recorded the approval. The order is the point:
    // a body handed to a local session before the trusted path accepted the
    // decision would be a delivery the audit log does not know about.
    let delivered = delivery.map(|(message_id, target)| {
        hand_to_herdr(
            &target,
            bodies
                .get(&message_id)
                .map(String::as_str)
                .unwrap_or_default(),
        )
    });

    Ok(json!({
        "status": "ok",
        "messageId": message_id,
        "decided": action,
        "daemon": applied,
        "delivered": delivered,
    }))
}

/// Every local session an approved message could be delivered to.
///
/// `fallback` is what [`local_agent`] read out of the plugin context, used
/// only when Herdr answers nothing: a single destination named the way the
/// person's own Herdr names it beats an empty list that makes delivery
/// impossible. It is marked not ready, because nothing has confirmed
/// anything can receive there.
fn destinations(fallback: &str) -> Vec<hrc_herdr::LocalAgent> {
    let listed = crate::herdr_host::Host::connect()
        .and_then(|mut host| host.destinations())
        .unwrap_or_default();

    if !listed.is_empty() {
        return listed;
    }

    vec![
        hrc_herdr::LocalAgent::new(fallback, fallback)
            .as_current(true)
            .when_ready(false),
    ]
}

/// Why a chosen destination can no longer receive, if it cannot.
///
/// `None` means go ahead: either Herdr still lists the target and says it can
/// take input, or Herdr could not be asked at all. The second case is
/// deliberate — an unreachable host is not evidence that a destination
/// vanished, and refusing a decision a person already made because a status
/// query failed would be the check doing more harm than the race it guards.
fn departed(target: &str) -> Option<CliError> {
    let Ok(listed) = crate::herdr_host::Host::connect().and_then(|mut host| host.destinations())
    else {
        return None;
    };

    // An empty listing from a reachable Herdr is not evidence either: the
    // fallback destination exists precisely because `agent.list` can be empty
    // while a session is still there.
    if listed.is_empty() {
        return None;
    }

    match listed.iter().find(|agent| agent.local_target() == target) {
        Some(agent) if agent.is_ready() => None,
        Some(agent) => Some(CliError::DestinationUnavailable {
            destination: agent.label().to_owned(),
            reason: "it cannot take input right now",
        }),
        None => Some(CliError::DestinationUnavailable {
            destination: target.to_owned(),
            reason: "it is no longer there",
        }),
    }
}

/// Hands approved text to the chosen local session.
///
/// Reports what happened rather than returning an error. The decision is
/// already recorded and is not undone by a delivery that did not land: the
/// message was approved, and a person who was told "approved" and then saw
/// an error would not know which of the two is true. What they need is both
/// facts, which is what this puts in the answer.
fn hand_to_herdr(target: &str, text: &str) -> Value {
    if text.is_empty() {
        return json!({
            "handedOver": false,
            "reason": "the approved content was not available to hand over",
        });
    }

    // Reconnected rather than reusing the connection the list came from,
    // because the screen may have been open for minutes and a server that
    // restarted in between would leave a dead socket behind.
    match crate::herdr_host::Host::connect().and_then(|mut host| host.deliver(target, text)) {
        Ok(()) => json!({ "handedOver": true }),
        Err(error) => json!({
            "handedOver": false,
            "reason": error.to_string(),
        }),
    }
}

/// Whether a human is actually at this terminal.
///
/// Both directions are checked. Output alone would pass for a process that
/// captured input and left the terminal attached to output; input alone
/// would pass for one that did the reverse.
fn is_a_terminal() -> bool {
    std::io::IsTerminal::is_terminal(&std::io::stdin())
        && std::io::IsTerminal::is_terminal(&std::io::stdout())
}

/// A single-threaded runtime for the trusted calls.
fn runtime() -> Result<tokio::runtime::Runtime> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| CliError::Io {
            action: "start the Tokio runtime",
            source,
        })
}

/// The daemon's trusted endpoint for this installation.
fn trusted_endpoint(context: &Context) -> Result<Endpoint> {
    Ok(Endpoint::new(
        &context.paths.runtime(),
        Interface::TrustedHuman,
    )?)
}

/// Reads every pending message the human may decide on, bodies included.
///
/// The split here is the whole design in miniature. The metadata comes from
/// local state, because it is the agent-safe view — the same closed set of
/// section 19.1 that an agent may already read. Each body comes from a
/// separate trusted call about a named message, because disclosing one is
/// exactly what the trusted interface is for. There is deliberately no bulk
/// "give me every pending body" method to reach for.
async fn collect_pending(
    context: &Context,
    endpoint: &Endpoint,
    only: Option<&str>,
) -> Result<Vec<PendingItem>> {
    let database = Database::open(context.paths.database())?;

    let mut waiting = Vec::new();
    for channel in database.channels()? {
        for entry in database.plugin_inbox(&channel.channel_id)? {
            if only.is_some_and(|wanted| wanted != entry.message_id) {
                continue;
            }

            // No local alias store exists yet, so the verified principal ID
            // stands in for the display name, exactly as the plugin pane
            // does. A sender-chosen name is never used.
            let view = hrc_herdr::agent_view(&entry, &entry.sender_principal, &channel.local_name);
            if !view.awaiting_decision {
                continue;
            }

            waiting.push((entry.message_id.clone(), view));
        }
    }

    if waiting.is_empty() {
        return Ok(Vec::new());
    }

    let mut client = connect(endpoint).await?;
    let mut items = Vec::new();
    for (message_id, view) in waiting {
        let previewed: Value = client
            .call(&TrustedRequest::PreviewPending {
                message_id: message_id.clone(),
            })
            .await
            .map_err(|_| CliError::DaemonUnavailable)?;

        // A message the daemon will not disclose is skipped rather than
        // shown with an empty body: a screen with nothing where a body
        // belongs invites a decision about content nobody read.
        let Some(body) = previewed["body"].as_str() else {
            continue;
        };

        items.push(PendingItem {
            message_id,
            view,
            body: body.to_owned(),
        });
    }

    Ok(items)
}

/// Connects to the trusted endpoint, or explains that the daemon is not up.
async fn connect(endpoint: &Endpoint) -> Result<Client> {
    Client::connect_with_retry(endpoint, CONNECT_ATTEMPTS, CONNECT_INTERVAL)
        .await
        .map_err(|_| CliError::DaemonUnavailable)
}

/// Turns a screen outcome into the decision the daemon records.
///
/// `Quit` yields nothing: leaving the screen is not a decision, and
/// recording one would put a choice in the audit log that nobody made.
fn wire_decision(outcome: Outcome) -> Option<(String, WireDecision, &'static str)> {
    match outcome {
        Outcome::DeliverToAgent {
            message_id, agent, ..
        } => Some((
            message_id,
            WireDecision::DeliverToAgent { agent },
            "deliver_to_agent",
        )),
        Outcome::KeepInInbox { message_id } => {
            Some((message_id, WireDecision::KeepInInbox, "keep_in_inbox"))
        }
        Outcome::Decline { message_id } => Some((
            message_id,
            // The screen does not collect a reason yet, and inventing one would
            // put words in the human's mouth on a message that goes back to
            // the sender.
            WireDecision::Decline { reason: None },
            "decline",
        )),
        Outcome::Quit => None,
    }
}

/// Hands the decision to the daemon's trusted interface.
async fn apply(endpoint: &Endpoint, message_id: &str, decision: WireDecision) -> Result<Value> {
    let mut client = connect(endpoint).await?;
    let expires_at = expiry()?;

    client
        .call(&TrustedRequest::Approve {
            message_id: message_id.to_owned(),
            decision,
            expires_at,
        })
        .await
        .map_err(CliError::from)
}

/// An RFC 3339 expiry for the approval authorization.
fn expiry() -> Result<String> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CliError::Entropy)?;

    Ok(crate::commands::rfc3339_from(
        now.as_secs() as i64 + AUTHORIZATION_LIFETIME_SECONDS,
    ))
}

/// The local agent an approved delivery would go to (PRD section 23.4).
///
/// Herdr injects `HERDR_PLUGIN_CONTEXT_JSON` describing the invocation, and
/// the focused agent or pane in it is the one the human is looking at, which
/// is the only sensible default for "deliver this to my agent".
///
/// This identifier is local and stays local. Section 4 constraint 8 and
/// section 23.4 both say remote participants never receive local pane or
/// agent identifiers, so it is read here, shown on the confirmation prompt,
/// and recorded in the local decision — never put on a message.
///
/// Falling back to a fixed label rather than guessing keeps the confirmation
/// honest: it names what it knows.
pub fn local_agent() -> String {
    const FALLBACK: &str = "this Herdr session";

    let Ok(raw) = std::env::var("HERDR_PLUGIN_CONTEXT_JSON") else {
        return FALLBACK.to_owned();
    };

    let Ok(context): std::result::Result<Value, _> = serde_json::from_str(&raw) else {
        return FALLBACK.to_owned();
    };

    // `agent` when Herdr knows one, the focused pane otherwise. Both are
    // host-supplied and neither crosses the channel.
    for key in ["agent", "focused_pane", "pane"] {
        if let Some(name) = context.get(key).and_then(agent_label) {
            return name;
        }
    }

    FALLBACK.to_owned()
}

/// Reads an agent or pane label out of whichever shape the host used.
///
/// Herdr documents the context as "workspace, tab, focused pane, worktree,
/// agent, selected text" without fixing each one's shape, so a bare string
/// and an object carrying a name or id are both accepted rather than
/// assuming one and silently falling back on the other.
fn agent_label(value: &Value) -> Option<String> {
    if let Some(name) = value.as_str() {
        return (!name.is_empty()).then(|| name.to_owned());
    }

    ["name", "title", "id"]
        .into_iter()
        .find_map(|key| value.get(key).and_then(Value::as_str))
        .filter(|name| !name.is_empty())
        .map(str::to_owned)
}

/// Opens the membership approval screen (PRD requirement HRC-CH-007).
///
/// The requests come from `join_pending`, which only returns those whose
/// proof already validates against an invite this installation issued. An
/// unverifiable request is not shown, because there is nothing a human could
/// usefully decide about one.
pub fn review_joins(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "review join requests")?;
    let listed = crate::commands::join_pending(context)?;
    let requests = pending_joins(&listed);

    if requests.is_empty() {
        return Ok(json!({
            "status": "ok",
            "pending": 0,
            "decided": Value::Null,
        }));
    }

    let channel_local_name = channel_label(context)?;
    let mut screen = JoinApp::new(requests, channel_local_name);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the membership approval screen",
        source,
    })?;

    let (request, admitted) = match outcome {
        JoinOutcome::Admit { request_id } => (
            TrustedRequest::ApproveJoin {
                request_id: request_id.clone(),
            },
            true,
        ),
        JoinOutcome::Refuse { request_id } => (
            TrustedRequest::RejectJoin {
                request_id: request_id.clone(),
            },
            false,
        ),
        // Leaving is not a decision, and recording one would put a choice in
        // the audit log that nobody made.
        JoinOutcome::Quit => {
            return Ok(json!({
                "status": "ok",
                "decided": Value::Null,
            }));
        }
    };

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;
    let applied: Value = runtime.block_on(async {
        let mut client = connect(&endpoint).await?;
        let response: Value = client.call(&request).await.map_err(CliError::from)?;
        Ok::<Value, CliError>(response)
    })?;

    Ok(json!({
        "status": "ok",
        "decided": if admitted { "admit" } else { "refuse" },
        "daemon": applied,
    }))
}

/// Reads the pending join requests out of a `join pending` result.
fn pending_joins(listed: &Value) -> Vec<PendingJoin> {
    listed["pending"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|entry| {
                    Some(PendingJoin {
                        request_id: entry["requestId"].as_str()?.to_owned(),
                        principal_id: entry["principalId"].as_str()?.to_owned(),
                        device_id: entry["deviceId"].as_str()?.to_owned(),
                        created_at: entry["createdAt"].as_str().unwrap_or_default().to_owned(),
                        safety_phrase: entry["safetyPhrase"].as_str()?.to_owned(),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The locally chosen name of the channel being admitted to.
///
/// Local rather than anything a remote party supplied: the confirmation
/// prompt names it, and a name a joiner could choose would be a name a
/// joiner could use to make admission read as routine.
fn channel_label(context: &Context) -> Result<String> {
    let database = Database::open(context.paths.database())?;
    let channel = crate::commands::only_channel(&database)?;
    Ok(channel.local_name)
}

/// Opens the composition screen.
///
/// Sending is not a trusted-human operation, so this needs no authorization;
/// it needs a terminal because it is a terminal interface. The refusal is the
/// same one the approval screens give, which is a little strong for what this
/// does, but a caller that wanted to send without a terminal already has
/// `hrc send` and the agent-safe `draft` method.
pub fn compose(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "send a message")?;
    let mut recipients = addressable(context)?;
    recipients.extend(answerable(context)?);

    if recipients.is_empty() {
        return Ok(json!({
            "status": "ok",
            "recipients": 0,
            "sent": Value::Null,
        }));
    }

    let mut screen = ComposeApp::new(recipients);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the composition screen",
        source,
    })?;

    let ComposeOutcome::Send {
        recipient,
        kind,
        text,
        in_reply_to,
    } = outcome
    else {
        return Ok(json!({
            "status": "ok",
            "sent": Value::Null,
        }));
    };

    // The same functions the CLI uses, rather than a second publication
    // path. A screen that composed its own envelopes would be a second place
    // where the chain, the roster epoch and the expiry rules live.
    //
    // A reply goes through `reply`, which reads the thread from the message
    // being answered rather than taking one. The kind is not consulted there:
    // an answer is an answer.
    match in_reply_to {
        Some(message_id) => crate::commands::reply(context, &message_id, &text),
        None => match kind {
            hrc_tui::ComposeKind::Note => crate::commands::send(context, &recipient, &text, None),
            hrc_tui::ComposeKind::Question => {
                crate::commands::ask(context, &recipient, &text, None)
            }
        },
    }
}

/// Messages that can be answered, as reply targets.
///
/// Only what arrived from someone else and is still awaiting a decision: a
/// message already declined or expired is not a conversation to continue, and
/// answering one this installation sent would be answering itself.
///
/// The body is not read and could not be: an unapproved body stays sealed.
/// What the list shows is the kind and the sender, which is the agent-safe
/// metadata the inbox already displays.
fn answerable(context: &Context) -> Result<Vec<Recipient>> {
    let database = Database::open(context.paths.database())?;
    let local = crate::commands::whoami(context)?;
    let local_principal = local["signingKey"].as_str().unwrap_or_default().to_owned();

    let mut targets = Vec::new();
    for channel in database.channels()? {
        // Per channel, because an alias is recorded per channel: the same
        // key verified in two places may have been named in only one.
        let aliases = database.principal_aliases(&channel.channel_id)?;

        for entry in database.inbox_entries(&channel.channel_id)? {
            if entry.sender_principal == local_principal {
                continue;
            }

            if !matches!(
                entry.disposition.as_str(),
                "quarantined" | "approved" | "edited"
            ) {
                continue;
            }

            targets.push(Recipient {
                display_name: aliases.get(&entry.sender_principal).cloned(),
                principal_id: entry.sender_principal,
                active: true,
                answering: Some(entry.kind),
                in_reply_to: Some(entry.message_id),
            });
        }
    }

    Ok(targets)
}

/// Everyone in the roster this installation can write to.
///
/// The local principal is filtered out. It is in the roster, and leaving it
/// in would make "send to myself" the preselected first entry on a screen
/// where the first entry is preselected.
fn addressable(context: &Context) -> Result<Vec<Recipient>> {
    let listed = crate::commands::members(context)?;
    let local = crate::commands::whoami(context)?;
    let local_principal = local["signingKey"].as_str().unwrap_or_default();

    // Read once, before the screen opens, so every row shows the name that
    // was on record when the person started looking at the roster.
    let aliases = {
        let database = Database::open(context.paths.database())?;
        let channel = crate::commands::only_channel(&database)?;
        database.principal_aliases(&channel.channel_id)?
    };

    Ok(listed["members"]
        .as_array()
        .map(|members| {
            members
                .iter()
                .filter_map(|member| {
                    let principal_id = member["principalId"].as_str()?;
                    if principal_id == local_principal {
                        return None;
                    }

                    Some(Recipient {
                        display_name: aliases.get(principal_id).cloned(),
                        principal_id: principal_id.to_owned(),
                        active: member["active"].as_bool().unwrap_or(false),
                        in_reply_to: None,
                        answering: None,
                    })
                })
                .collect()
        })
        .unwrap_or_default())
}

/// Opens the channel setup screen.
///
/// Which steps it offers depends on what this installation already has. A
/// step that can only fail is not offered: inviting needs a channel, and
/// creating one is pointless when a channel already exists.
pub fn setup(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let steps = available_steps(context)?;

    // Initialization is the one step that cannot ask for the passphrase
    // first, because it is the step that chooses it. Everything else needs
    // the key store open before it can do anything.
    let initializing = steps == [hrc_tui::SetupStep::Initialize];
    let owned;
    let context = if initializing {
        context
    } else {
        owned = unlocked(context, "set up this channel")?;
        &owned
    };

    let mut screen = SetupApp::new(steps);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the channel setup screen",
        source,
    })?;

    // Each arm calls the same function the CLI does, so there is one
    // implementation of what creating, inviting and joining mean.
    match outcome {
        SetupOutcome::Initialize { passphrase } => {
            let context = Context {
                paths: context.paths.clone(),
                passphrase: Some(SecretString::from(passphrase)),
            };
            crate::commands::init(&context)
        }
        SetupOutcome::CreateChannel { repo } => crate::commands::create(context, &repo, None),
        SetupOutcome::CreateInvite { github_user } => {
            crate::commands::invite_create(context, &github_user, DEFAULT_INVITE_LIFETIME)
        }
        SetupOutcome::Join { invite_code } => crate::commands::join(context, &invite_code),
        SetupOutcome::Quit => Ok(json!({
            "status": "ok",
            "done": Value::Null,
        })),
    }
}

/// How long an invite issued from the setup screen stays usable.
///
/// The CLI makes this an argument. The screen does not ask, because a
/// lifetime is a question most people cannot answer usefully at the moment
/// they are trying to invite someone, and a day is long enough to pass a
/// code along and short enough that a forgotten one lapses.
const DEFAULT_INVITE_LIFETIME: &str = "24h";

/// The setup steps that make sense for this installation right now.
fn available_steps(context: &Context) -> Result<Vec<hrc_tui::SetupStep>> {
    // Before there are keys there is nothing else to offer: every other step
    // signs something.
    //
    // The directory alone is not the test. `paths.ensure` creates it, so an
    // installation that has merely been looked at has one; what decides this
    // is whether anything is in it. Reading the store itself would need the
    // passphrase, which is the thing this step exists to choose.
    let initialized = std::fs::read_dir(context.paths.keys())
        .map(|mut entries| entries.next().is_some())
        .unwrap_or(false);

    if !initialized {
        return Ok(vec![hrc_tui::SetupStep::Initialize]);
    }

    let database = Database::open(context.paths.database())?;
    let has_channel = !database.channels()?.is_empty();

    Ok(if has_channel {
        // Joining again would mean a second channel, which the rest of the
        // CLI does not support yet: `only_channel` refuses when there is
        // more than one.
        vec![hrc_tui::SetupStep::CreateInvite]
    } else {
        vec![hrc_tui::SetupStep::CreateChannel, hrc_tui::SetupStep::Join]
    })
}

/// A context whose key store can be opened, asking for the passphrase if the
/// environment did not supply one.
///
/// `HRC_PASSPHRASE` still works and still wins, because scripts and the
/// daemon depend on it. What this adds is that a person in Herdr does not
/// have to export a secret into their environment to use the plugin — an
/// environment variable is inherited by every child process, readable from
/// `/proc` on Linux by anything running as the same user, and captured in
/// shell history when it is set by hand.
///
/// The passphrase lives for this one invocation and is not stored. That is a
/// stopgap rather than an answer: the answer is the OS keychain (decision
/// DEC-051), which is still awaiting a product-owner view.
fn unlocked(context: &Context, reason: &str) -> Result<Context> {
    if context.passphrase.is_some() {
        return Ok(context.clone());
    }

    let mut prompt = PassphraseApp::new(reason);
    let outcome = hrc_tui::run(&mut prompt).map_err(|source| CliError::Io {
        action: "ask for the key store passphrase",
        source,
    })?;

    match outcome {
        PassphraseOutcome::Unlock(passphrase) => Ok(Context {
            paths: context.paths.clone(),
            passphrase: Some(SecretString::from(passphrase)),
        }),
        PassphraseOutcome::Quit => Err(CliError::NoPassphrase),
    }
}

/// Opens the context disclosure screen (PRD sections 20 and 22.4).
///
/// Both halves go through the daemon's trusted interface rather than the
/// local functions of the same name, because doing the work locally would
/// move the boundary into a process an agent can start.
///
/// Section 22.4's context authorization is different from the prompt gate's.
/// The approval one never leaves the daemon (decision DEC-040); this one is
/// returned by `PreviewContext` and must be presented to `SendContext`, and
/// it is bound to the package digest, the recipient, the channel and the send
/// action, with a five-minute server-controlled expiry. So it is held here,
/// in memory, between the human seeing the preview and answering the
/// confirmation — and it is dropped the moment the selection changes, because
/// an authorization for bytes nobody is looking at any more is one nobody
/// meant to spend.
pub fn context(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "send a context package")?;

    let database = Database::open(context.paths.database())?;
    let drafts: Vec<hrc_tui::ContextDraft> = database
        .context_drafts()?
        .into_iter()
        .map(|(package_id, digest)| hrc_tui::ContextDraft { package_id, digest })
        .collect();

    if drafts.is_empty() {
        return Ok(json!({
            "status": "ok",
            "drafts": 0,
            "sent": Value::Null,
        }));
    }

    let recipients: Vec<String> = addressable(context)?
        .into_iter()
        .filter(|recipient| recipient.active)
        .map(|recipient| recipient.principal_id)
        .collect();

    if recipients.is_empty() {
        return Ok(json!({
            "status": "ok",
            "recipients": 0,
            "sent": Value::Null,
        }));
    }

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;
    let mut screen = ContextApp::new(drafts, recipients);
    let mut authorization: Option<String> = None;

    // The screen asks and answers more than once: a preview comes back into
    // it, and only then can a send be proposed. So the loop lives here rather
    // than in `hrc_tui::run`, which returns on the first outcome.
    loop {
        let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
            action: "run the context disclosure screen",
            source,
        })?;

        match outcome {
            ContextOutcome::Preview {
                package_id,
                recipient,
            } => {
                let previewed = runtime.block_on(trusted_call(
                    &endpoint,
                    TrustedRequest::PreviewContext {
                        recipient,
                        package_id: package_id.clone(),
                    },
                ))?;

                let parsed = preview_from(&previewed, &package_id)?;
                authorization = Some(parsed.authorization);
                screen.show_preview(parsed.preview);
            }

            ContextOutcome::Send {
                package_id,
                recipient,
            } => {
                // The authorization the preview issued for exactly this
                // package and recipient. Taken rather than cloned, so a
                // second send cannot reuse it from here — the daemon would
                // refuse it anyway, and the two agreeing is the point.
                let Some(granted) = authorization.take() else {
                    return Err(CliError::NoContextAuthorization);
                };

                let sent = runtime.block_on(trusted_call(
                    &endpoint,
                    TrustedRequest::SendContext {
                        recipient: recipient.clone(),
                        package_id: package_id.clone(),
                        authorization: granted,
                    },
                ))?;
                require_trusted_success(&sent, "send_context")?;

                return Ok(json!({
                    "status": "ok",
                    "contextId": package_id,
                    "recipient": recipient,
                    "daemon": sent,
                }));
            }

            ContextOutcome::Quit => {
                return Ok(json!({
                    "status": "ok",
                    "sent": Value::Null,
                }));
            }
        }
    }
}

/// One request and answer on the trusted endpoint.
async fn trusted_call(endpoint: &Endpoint, request: TrustedRequest) -> Result<Value> {
    let mut client = connect(endpoint).await?;
    client.call(&request).await.map_err(CliError::from)
}

/// Reads a successful trusted preview into the exact content the screen displays.
///
/// The daemon has already re-read source files and rejected secret or path
/// findings before it returns success. Missing fields and explicit error
/// responses therefore fail the command rather than becoming an empty,
/// apparently harmless preview.
fn preview_from(response: &Value, expected_package_id: &str) -> Result<ParsedContextPreview> {
    require_trusted_success(response, "preview_context")?;

    let package_id = required_string(response, "contextId")?;
    if package_id != expected_package_id {
        return Err(invalid_trusted_response(format!(
            "trusted preview answered for context `{package_id}` instead of `{expected_package_id}`"
        )));
    }

    let digest = required_string(response, "digest")?.to_owned();
    let content = required_string(response, "content")?.to_owned();
    let authorization = required_string(response, "authorization")?.to_owned();
    let total_bytes = response["totalBytes"].as_u64().ok_or_else(|| {
        invalid_trusted_response("trusted preview omitted the canonical package size")
    })?;
    if total_bytes != content.len() as u64 {
        return Err(invalid_trusted_response(format!(
            "trusted preview declared {total_bytes} bytes but returned {}",
            content.len()
        )));
    }

    let package = hrc_protocol::canonical::from_json_str::<hrc_core::ContextPackage>(&content)
        .map_err(|error| {
            invalid_trusted_response(format!(
                "trusted preview returned invalid canonical content: {error}"
            ))
        })?;
    if package.id != package_id {
        return Err(invalid_trusted_response(
            "trusted preview content has a different package identifier",
        ));
    }
    let canonical_content =
        hrc_protocol::canonical::to_canonical_bytes(&package).map_err(|error| {
            invalid_trusted_response(format!(
                "trusted preview content could not be canonicalized: {error}"
            ))
        })?;
    if canonical_content.as_slice() != content.as_bytes() {
        return Err(invalid_trusted_response(
            "trusted preview content was not the canonical package representation",
        ));
    }
    package.verify_digest(&digest).map_err(|error| {
        invalid_trusted_response(format!(
            "trusted preview content does not match its digest: {error}"
        ))
    })?;

    let items = response["items"]
        .as_array()
        .ok_or_else(|| invalid_trusted_response("trusted preview omitted its item list"))?
        .iter()
        .map(parse_preview_item)
        .collect::<Result<Vec<_>>>()?;
    let verified = package.preview().map_err(|error| {
        invalid_trusted_response(format!(
            "trusted preview content could not be verified: {error}"
        ))
    })?;
    if !verified.is_sendable() {
        return Err(invalid_trusted_response(
            "trusted preview returned content that failed local disclosure checks",
        ));
    }
    let verified_items = verified
        .items
        .into_iter()
        .map(|(kind, bytes)| (kind, bytes as u64))
        .collect::<Vec<_>>();
    if items != verified_items || total_bytes != verified.total_bytes as u64 {
        return Err(invalid_trusted_response(
            "trusted preview summary did not match its canonical content",
        ));
    }

    Ok(ParsedContextPreview {
        authorization,
        preview: hrc_tui::ContextPreview {
            digest,
            content,
            items,
            total_bytes,
            secrets: Vec::new(),
            excluded: Vec::new(),
            // A successful daemon preview has already re-read the source,
            // checked exclusions and scanned secrets. Error responses never
            // reach the screen through this parser.
            sendable: true,
        },
    })
}

#[derive(Debug)]
struct ParsedContextPreview {
    authorization: String,
    preview: hrc_tui::ContextPreview,
}

fn parse_preview_item(value: &Value) -> Result<(String, u64)> {
    if let Some(pair) = value.as_array()
        && pair.len() == 2
        && let (Some(kind), Some(bytes)) = (pair[0].as_str(), pair[1].as_u64())
    {
        return Ok((kind.to_owned(), bytes));
    }

    if let (Some(kind), Some(bytes)) = (value["kind"].as_str(), value["bytes"].as_u64()) {
        return Ok((kind.to_owned(), bytes));
    }

    Err(invalid_trusted_response(
        "trusted preview returned a malformed item summary",
    ))
}

fn require_trusted_success(response: &Value, expected_method: &str) -> Result<()> {
    if response["status"] != "ok" {
        let code = response["code"].as_str().unwrap_or("unknown_error");
        let message = response["message"]
            .as_str()
            .unwrap_or("the daemon did not explain the failure");
        return Err(invalid_trusted_response(format!(
            "trusted `{expected_method}` failed with `{code}`: {message}"
        )));
    }

    if response["method"].as_str() != Some(expected_method) {
        return Err(invalid_trusted_response(format!(
            "trusted response did not identify `{expected_method}`"
        )));
    }

    Ok(())
}

fn required_string<'a>(response: &'a Value, key: &str) -> Result<&'a str> {
    response[key]
        .as_str()
        .filter(|value| !value.is_empty())
        .ok_or_else(|| invalid_trusted_response(format!("trusted preview omitted `{key}`")))
}

fn invalid_trusted_response(reason: impl Into<String>) -> CliError {
    CliError::Ipc(hrc_ipc::IpcError::Malformed(reason.into()))
}

/// Opens the membership screen (PRD requirement HRC-CH-008).
///
/// Removing a member and revoking a device are section 22.7 operations, and
/// both publish a control entry to an append-only history that advances the
/// roster epoch. Nothing here can take one back, which is why the screen says
/// so before it asks.
pub fn members(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return Err(CliError::NotInteractive);
    }

    let context = &unlocked(context, "change who is in this channel")?;

    let listed = crate::commands::members(context)?;
    let local = crate::commands::whoami(context)?;
    let local_principal = local["signingKey"].as_str().unwrap_or_default();

    // Read once, before the screen opens, so every row shows the name that
    // was on record when the person started looking at the roster.
    let aliases = {
        let database = Database::open(context.paths.database())?;
        let channel = crate::commands::only_channel(&database)?;
        database.principal_aliases(&channel.channel_id)?
    };

    let roster: Vec<hrc_tui::Member> = listed["members"]
        .as_array()
        .map(|members| {
            members
                .iter()
                .filter_map(|member| {
                    let principal_id = member["principalId"].as_str()?.to_owned();
                    let devices = member["devices"]
                        .as_array()
                        .map(|devices| {
                            devices
                                .iter()
                                .filter_map(|device| {
                                    Some(hrc_tui::MemberDevice {
                                        device_id: device["deviceId"].as_str()?.to_owned(),
                                        active: device["active"].as_bool().unwrap_or(false),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    Some(hrc_tui::Member {
                        // Told rather than guessed: it is what lets the screen
                        // refuse to remove this installation.
                        is_local: principal_id == local_principal,
                        display_name: aliases.get(&principal_id).cloned(),
                        principal_id,
                        administrator: member["administrator"].as_bool().unwrap_or(false),
                        active: member["active"].as_bool().unwrap_or(false),
                        devices,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    if roster.is_empty() {
        return Ok(json!({
            "status": "ok",
            "members": 0,
            "changed": Value::Null,
        }));
    }

    let mut screen = MembersApp::new(roster);
    let outcome = hrc_tui::run(&mut screen).map_err(|source| CliError::Io {
        action: "run the membership screen",
        source,
    })?;

    let (request, changed) = match outcome {
        MemberOutcome::Remove { principal_id } => (
            TrustedRequest::RemoveMember {
                principal_id: principal_id.clone(),
            },
            json!({ "removed": principal_id }),
        ),
        MemberOutcome::Revoke { device_id } => (
            TrustedRequest::RevokeDevice {
                device_id: device_id.clone(),
            },
            json!({ "revoked": device_id }),
        ),
        MemberOutcome::Name {
            principal_id,
            display_name,
        } => (
            TrustedRequest::SetAlias {
                principal_id: principal_id.clone(),
                display_name: display_name.clone(),
            },
            // The principal is named, not the name: this answer is written
            // to a log and to an agent-readable surface, and the point of
            // the alias is that it stays on the screen a human is looking
            // at.
            json!({ "named": principal_id, "cleared": display_name.is_none() }),
        ),
        MemberOutcome::Quit => {
            return Ok(json!({
                "status": "ok",
                "changed": Value::Null,
            }));
        }
    };

    let runtime = runtime()?;
    let endpoint = trusted_endpoint(context)?;
    let applied = runtime.block_on(trusted_call(&endpoint, request))?;

    Ok(json!({
        "status": "ok",
        "changed": changed,
        "daemon": applied,
    }))
}

/// How often the inbox side view reloads its rows.
///
/// A second is fast enough that an arrival appears while a person is still
/// looking at the pane, and slow enough that the cost is one indexed read of
/// a local SQLite file per second. The alternative the PRD rules out is a
/// frequently fired Herdr host event, which would spawn a process per tick
/// (section 13.2).
const INBOX_REFRESH: Duration = Duration::from_secs(1);

/// The inbox side view, wired to storage and to the Herdr host.
///
/// [`hrc_tui::InboxApp`] is the state machine and knows nothing about either.
/// This owns the database handle and the path back to Herdr, which is what
/// keeps reloading out of the UI and makes the UI testable without a
/// database.
struct InboxScreen {
    app: hrc_tui::InboxApp,
    database: Database,
    channel_id: String,
    channel_local_name: String,
    now: String,
    /// Each message the popup was opened for, in order, so the command can
    /// report what a person actually looked at.
    reviewed: Vec<String>,
}

impl InboxScreen {
    /// Reloads the rows, keeping the last good ones when the read fails.
    fn reload(&mut self) {
        match inbox_rows(&self.database, &self.channel_id, &self.channel_local_name) {
            Ok((rows, now)) => {
                self.now = now;
                self.announce(&rows);
                self.app.refresh(rows);
                // The section 23.1 indicator, on the surface it describes.
                if let Ok(health) = crate::commands::herdr_sidebar_for(&self.database) {
                    self.app.set_health(health);
                }
            }
            // A failed read must not look like an empty inbox. The rows
            // already on screen stay, and the status line says why they may
            // be out of date.
            Err(error) => self.app.refresh_failed(error.to_string()),
        }
    }

    /// Raises a Herdr notification for anything newly actionable.
    ///
    /// The ledger decides what is new, so a pane refreshing once a second
    /// raises nothing on the refreshes where nothing changed — which is
    /// almost all of them, and is the behaviour people turn notifications
    /// off over when it is missing.
    ///
    /// A failure here is deliberately swallowed. The message is in the inbox
    /// whether or not a toast appeared, and the pane in front of the person
    /// already shows it; failing a refresh because a notification did not
    /// land would replace a missing toast with a missing inbox.
    fn announce(&mut self, rows: &[hrc_herdr::InboxRow]) {
        let Ok(notices) = raise_notifications(&self.database, &self.channel_id, rows) else {
            return;
        };

        if notices.is_empty() {
            return;
        }

        let Ok(mut host) = crate::herdr_host::Host::connect() else {
            return;
        };

        for notice in notices {
            let (Some(text), Some(urgent)) = (
                notice.get("text").and_then(Value::as_str),
                notice.get("urgent").and_then(Value::as_bool),
            ) else {
                continue;
            };

            let _ = host.notify(text, urgent);
        }
    }

    /// Asks Herdr to open the trusted review popup on one message.
    ///
    /// The side view stays open behind it. Returning the outcome from the
    /// runner instead would restore the terminal and exit, which in a Herdr
    /// split means closing the pane the person was working out of.
    fn open_review(&mut self, message_id: &str) {
        // Only a well-formed ULID is handed to the host. A `messageId`
        // arrives in the envelope and the protocol checks only that it is
        // non-empty, so it is sender-chosen text; the popup opens on the
        // unfocused pending list rather than carrying that text onto a
        // command line (decision DEC-087).
        if !hrc_protocol::is_ulid(message_id) {
            self.status("That message has a malformed identifier. Opening the full pending list.");
            self.spawn_review(None);
            return;
        }

        self.reviewed.push(message_id.to_owned());
        self.spawn_review(Some(message_id));
    }

    /// Runs `herdr plugin pane open`, reporting a refusal rather than hiding it.
    fn spawn_review(&mut self, message_id: Option<&str>) {
        let herdr = std::env::var("HERDR_BIN_PATH").unwrap_or_else(|_| "herdr".to_owned());
        let arguments = match message_id {
            Some(message_id) => hrc_herdr::open_review(message_id),
            None => hrc_herdr::open_review_list(),
        };

        match std::process::Command::new(&herdr)
            .args(&arguments)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .output()
        {
            Ok(output) if output.status.success() => {}
            // Herdr answers `ui_busy` while Settings, Copy mode, or another
            // modal is up. Saying so leaves the person able to dismiss that
            // and press Enter again; a silent failure would look like a pane
            // that ignores the key.
            Ok(output) => {
                let reason = String::from_utf8_lossy(&output.stderr);
                let reason = reason.trim();
                self.status(&format!(
                    "Herdr did not open the review popup{}{}",
                    if reason.is_empty() { "" } else { ": " },
                    reason
                ));
            }
            Err(error) => self.status(&format!("Could not run {herdr}: {error}")),
        }
    }

    /// Puts one fixed local line in front of the person.
    fn status(&mut self, text: &str) {
        self.app.refresh_failed(text.to_owned());
    }
}

impl hrc_tui::Screen for InboxScreen {
    type Outcome = hrc_tui::InboxOutcome;

    fn draw(&self, frame: &mut hrc_tui::ratatui::Frame<'_>) {
        hrc_tui::render_inbox(frame, &self.app, &self.now);
    }

    fn on_key(&mut self, key: hrc_tui::crossterm::event::KeyEvent) -> Option<Self::Outcome> {
        let outcome = self.app.on_key(key);

        if self.app.refresh_requested() {
            self.reload();
        }

        match outcome {
            Some(hrc_tui::InboxOutcome::Review { message_id }) => {
                self.open_review(&message_id);
                // Deliberately not propagated. The only outcome that ends the
                // side view is leaving it.
                None
            }
            Some(hrc_tui::InboxOutcome::Quit) => Some(hrc_tui::InboxOutcome::Quit),
            None => None,
        }
    }
}

impl hrc_tui::Ticking for InboxScreen {
    fn on_tick(&mut self) -> Option<Self::Outcome> {
        self.reload();
        None
    }
}

/// Reads the inbox rows for the one configured channel, with the time used
/// to judge expiry and render ages.
fn inbox_rows(
    database: &Database,
    channel_id: &str,
    channel_local_name: &str,
) -> Result<(Vec<hrc_herdr::InboxRow>, String)> {
    let now = database.utc_now()?;

    // Shortened once, here, so the pane title, every row and every
    // notification agree on what this channel is called. A notification is
    // capped at eighty characters by the host, and a locator spent most of
    // them (decision DEC-095).
    let channel_local_name = &hrc_herdr::channel_display_name(channel_local_name);

    // Read once for the whole render, so every row on screen resolves its
    // sender against the same snapshot. A name that changed halfway down a
    // list would be the surface disagreeing with itself.
    let aliases = database.principal_aliases(channel_id)?;

    let rows = database
        .plugin_inbox(channel_id)?
        .iter()
        .map(|entry| {
            // The name the local human assigned when they verified this
            // person, falling back to a shortened principal ID. Both are
            // locally resolved, which is what section 19.1 requires; neither
            // is a name the sender chose, which is what it forbids.
            let sender = hrc_herdr::principal_display_name(
                &entry.sender_principal,
                aliases.get(&entry.sender_principal).map(String::as_str),
            );

            hrc_herdr::InboxRow::from_entry(entry, &sender, channel_local_name, &now)
        })
        .collect();

    Ok((hrc_herdr::InboxView::new(rows).rows, now))
}

/// Builds the inbox snapshot a non-interactive caller receives.
///
/// The same rows the side view draws, as JSON. This is the agent-safe
/// surface and stays exactly as wide as it was: metadata only, no body, no
/// field that could hold one.
pub fn inbox_snapshot(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = crate::commands::only_channel(&database)?;
    let (rows, _) = inbox_rows(&database, &channel.channel_id, &channel.local_name)?;

    let notifications = raise_notifications(&database, &channel.channel_id, &rows)?;
    let view = hrc_herdr::InboxView::new(rows);

    Ok(json!({
        "status": "ok",
        "pane": "inbox",
        "channel": channel.local_name,
        "pending": view.pending(),
        "rows": view.rows,
        "notifications": notifications,
    }))
}

/// The notices this refresh should raise, recording that it raised them.
///
/// Called on every read of the inbox, which is once a second while the side
/// view is open. What makes that safe is that the ledger is in the database
/// rather than in this process: `record_notification` returns true the first
/// time a message-and-kind pair is seen and false every time after, across
/// refreshes and across restarts.
///
/// Messages that are no longer awaiting a decision have their notices
/// resolved in the same pass. A person who declined something should not be
/// told about it again by a pane that has not caught up, and a resolved row
/// stays in the ledger so that being decided is distinguishable from never
/// having been notified.
fn raise_notifications(
    database: &Database,
    channel_id: &str,
    rows: &[hrc_herdr::InboxRow],
) -> Result<Vec<Value>> {
    let now = database.utc_now()?;
    let mut fresh = Vec::new();

    for row in rows {
        let Some(notification) = hrc_herdr::Notification::for_message(row) else {
            continue;
        };

        if !row.awaiting_decision() {
            database.resolve_notifications(&row.message_id, &now)?;
            continue;
        }

        if database.record_notification(channel_id, &row.message_id, notification.kind(), &now)? {
            fresh.push(notification);
        }
    }

    Ok(hrc_herdr::coalesce(fresh)
        .into_iter()
        .map(|notice| {
            json!({
                "text": notice.text,
                "urgent": notice.urgent,
                "covers": notice.covers,
            })
        })
        .collect())
}

/// Opens the inbox side view (PRD section 23.2).
///
/// Interactive when a person is at the terminal, and the JSON snapshot
/// otherwise. Both show the same closed metadata set, so this is a choice
/// about presentation rather than about what is disclosed: a pipe gets rows
/// it can parse, a person gets rows they can move through. Refusing the
/// non-interactive case would break the agent-safe read that section 19.1
/// exists to allow, which is the opposite of what section 22.7 protects.
pub fn inbox(context: &Context) -> Result<Value> {
    if !is_a_terminal() {
        return inbox_snapshot(context);
    }

    let database = Database::open(context.paths.database())?;
    let channel = crate::commands::only_channel(&database)?;
    let (rows, now) = inbox_rows(&database, &channel.channel_id, &channel.local_name)?;

    let mut screen = InboxScreen {
        app: hrc_tui::InboxApp::new(channel.local_name.clone(), rows),
        database,
        channel_id: channel.channel_id.clone(),
        channel_local_name: channel.local_name.clone(),
        now,
        reviewed: Vec::new(),
    };

    hrc_tui::run_ticking(&mut screen, INBOX_REFRESH).map_err(|source| CliError::Io {
        action: "run the inbox side view",
        source,
    })?;

    Ok(json!({
        "status": "ok",
        "pane": "inbox",
        "channel": channel.local_name,
        "pending": screen.app.pending(),
        "reviewed": screen.reviewed,
    }))
}

/// The message the review popup was opened for, if Herdr passed one.
///
/// Read from the environment Herdr set on this process alone, so it cannot
/// come from another workspace, another popup, or a file a second Herdr
/// session could reach. A value that is not a well-formed ULID is dropped
/// rather than used: the popup then lists everything pending, which is the
/// unfocused screen a person can still work from, not a wrong message
/// (decision DEC-087).
pub fn review_target() -> Option<String> {
    review_target_from(std::env::var(hrc_herdr::REVIEW_TARGET_ENV).ok())
}

/// The checking half of [`review_target`], separated from the environment.
///
/// Reading a process environment variable in a test means mutating global
/// state that every other test in the binary shares, which `unsafe_code =
/// "forbid"` rules out here anyway. Splitting the decision from where the
/// value came from lets the rule that matters — missing, empty, malformed
/// and already-consumed all fail closed — be tested directly.
fn review_target_from(value: Option<String>) -> Option<String> {
    let target = value?;
    hrc_protocol::is_ulid(&target).then_some(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_departed_destination_is_reported_by_the_name_the_person_saw() {
        // The error is raised before the daemon is asked, so nothing is
        // recorded and the message stays pending. What it must not do is name
        // the pane: PRD section 23.4 keeps local topology local, and an error
        // string is as public as any other output.
        let gone = CliError::DestinationUnavailable {
            destination: "reviewer".to_owned(),
            reason: "it is no longer there",
        };

        let rendered = gone.to_string();
        assert!(rendered.contains("reviewer"), "{rendered}");
        assert!(
            rendered.contains("still waiting in your inbox"),
            "{rendered}"
        );
        assert!(!rendered.contains("w1:p"), "{rendered}");
        assert_eq!(gone.code(), "destination_unavailable");
    }

    #[test]
    fn an_unreachable_herdr_is_not_evidence_that_a_destination_vanished() {
        // `departed` guards a race, and a status query that could not run is
        // not the race happening. Refusing a decision a person already made
        // because Herdr could not be asked would be the check doing more harm
        // than the thing it guards against.
        //
        // This process is not running under Herdr, so `Host::connect` fails
        // and the guard must wave the delivery through.
        assert!(
            super::departed("reviewer").is_none(),
            "an unreachable host must not block a decision"
        );
    }

    #[test]
    fn a_review_target_that_is_not_a_ulid_opens_the_unfocused_list() {
        // Everything that is not a well-formed identifier fails the same
        // way, and failing means "list what is pending" rather than "review
        // something else". A wrong body in a trusted popup is the one
        // outcome this must never produce.
        assert_eq!(
            review_target_from(Some("01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned())).as_deref(),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV")
        );

        for malformed in [
            None,
            Some(String::new()),
            Some("  ".to_owned()),
            Some("not-a-ulid".to_owned()),
            // Long enough, wrong alphabet: `I`, `L`, `O` and `U` are not
            // Crockford base32.
            Some("01ARZ3NDEKTSV4RRFFQ69G5FIU".to_owned()),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV extra".to_owned()),
            Some("01ARZ3NDEKTSV4RRFFQ69G5FAV\nHRC_PASSPHRASE=x".to_owned()),
        ] {
            assert_eq!(
                review_target_from(malformed.clone()),
                None,
                "{malformed:?} should not target a message"
            );
        }
    }

    use std::process::Command as ProcessCommand;

    use crate::paths::Paths;

    fn test_context(root: &std::path::Path) -> Context {
        Context {
            paths: Paths::at(root.join("state")),
            passphrase: Some(SecretString::from(
                "correct horse battery staple".to_owned(),
            )),
        }
    }

    #[test]
    fn actual_daemon_preview_parses_into_a_sendable_visible_snapshot() {
        let directory = tempfile::tempdir().unwrap();
        let context = test_context(directory.path());
        super::super::init(&context).unwrap();

        let remote = directory.path().join("remote.git");
        let status = ProcessCommand::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&remote)
            .status()
            .unwrap();
        assert!(status.success());
        super::super::create(
            &context,
            remote.to_str().unwrap(),
            Some("Context preview test"),
        )
        .unwrap();

        let peer = test_context(&directory.path().join("peer"));
        let peer_identity = super::super::init(&peer).unwrap();
        let recipient = peer_identity["principalKey"].as_str().unwrap().to_owned();
        let invite = super::super::invite_create(&context, "peer", "24h").unwrap();
        let joined = super::super::join(&peer, invite["inviteCode"].as_str().unwrap()).unwrap();
        super::super::admit_join(&context, joined["requestId"].as_str().unwrap()).unwrap();

        let manifest = directory.path().join("context.json");
        let canonical_text = "review this exact canonical content";
        std::fs::write(
            &manifest,
            format!(
                r#"{{"version":1,"id":"ctx-live","items":[{{"kind":"note","text":"{canonical_text}"}}]}}"#
            ),
        )
        .unwrap();
        super::super::context_draft(&context, manifest.to_str().unwrap(), None).unwrap();

        let ledger = std::sync::Mutex::new(hrc_core::rpc::ContextAuthorizationLedger::new());
        let response = super::super::handle_trusted_request(
            &context,
            &ledger,
            TrustedRequest::PreviewContext {
                recipient: recipient.clone(),
                package_id: "ctx-live".into(),
            },
        );

        assert!(response.get("sendable").is_none());
        let parsed = preview_from(&response, "ctx-live").unwrap();
        assert!(!parsed.authorization.is_empty());
        assert!(parsed.preview.sendable);
        assert!(parsed.preview.content.contains(canonical_text));
        assert_eq!(parsed.preview.items.len(), 1);
        assert_eq!(parsed.preview.items[0].0, "note");
        assert_eq!(
            parsed.preview.total_bytes,
            parsed.preview.content.len() as u64
        );
        let authorization = parsed.authorization.clone();

        let mut screen = ContextApp::new(
            vec![hrc_tui::ContextDraft {
                package_id: "ctx-live".into(),
                digest: parsed.preview.digest.clone(),
            }],
            vec![recipient.clone()],
        );
        screen.show_preview(parsed.preview);
        assert!(screen.status().contains("Ctrl+S"));
        assert!(screen.preview().unwrap().content.contains(canonical_text));

        let sent = super::super::handle_trusted_request(
            &context,
            &ledger,
            TrustedRequest::SendContext {
                recipient,
                package_id: "ctx-live".into(),
                authorization,
            },
        );
        require_trusted_success(&sent, "send_context").unwrap();
    }

    #[test]
    fn daemon_error_response_is_not_rendered_as_an_empty_preview() {
        let response = json!({
            "status": "error",
            "code": "core_error",
            "message": "context package is blocked by source or secret checks",
        });

        let error = preview_from(&response, "ctx-blocked").unwrap_err();
        assert!(
            error
                .to_string()
                .contains("blocked by source or secret checks"),
            "{error}"
        );
    }
}
