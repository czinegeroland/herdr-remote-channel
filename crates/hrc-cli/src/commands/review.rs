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

    let mut app = App::new(pending, agent);
    let outcome = hrc_tui::run(&mut app).map_err(|source| CliError::Io {
        action: "run the trusted approval screen",
        source,
    })?;

    let Some((message_id, decision, action)) = wire_decision(outcome) else {
        return Ok(json!({
            "status": "ok",
            "decided": Value::Null,
        }));
    };

    let applied = runtime.block_on(apply(&endpoint, &message_id, decision))?;

    Ok(json!({
        "status": "ok",
        "messageId": message_id,
        "decided": action,
        "daemon": applied,
    }))
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
        Outcome::DeliverToAgent { message_id, agent } => Some((
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
                        package_id,
                    },
                ))?;

                authorization = previewed["authorization"].as_str().map(str::to_owned);
                screen.show_preview(preview_from(&previewed));
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

/// Reads a trusted preview response into what the screen displays.
///
/// A response the screen cannot parse yields a preview that is not sendable,
/// rather than an empty one that looks harmless. The failure mode to avoid
/// here is a screen that shows "0 bytes, no findings" because a field moved.
fn preview_from(response: &Value) -> hrc_tui::ContextPreview {
    let strings = |key: &str, render: fn(&Value) -> Option<String>| -> Vec<String> {
        response[key]
            .as_array()
            .map(|entries| entries.iter().filter_map(render).collect())
            .unwrap_or_default()
    };

    let secrets = strings("secretFindings", |finding| {
        Some(format!(
            "item {}: {}",
            finding["item"].as_u64()?,
            finding["rule"].as_str()?
        ))
    });

    let excluded = strings("excludedPaths", |path| {
        Some(format!(
            "{} ({})",
            path["path"].as_str()?,
            path["reason"].as_str().unwrap_or("excluded")
        ))
    });

    let items = response["items"]
        .as_array()
        .map(|entries| {
            entries
                .iter()
                .filter_map(|item| {
                    Some((item["kind"].as_str()?.to_owned(), item["bytes"].as_u64()?))
                })
                .collect()
        })
        .unwrap_or_default();

    let understood = response["status"] == "ok" && response["digest"].is_string();

    hrc_tui::ContextPreview {
        items,
        total_bytes: response["totalBytes"].as_u64().unwrap_or_default(),
        secrets,
        excluded,
        sendable: understood && response["sendable"].as_bool().unwrap_or(false),
    }
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
