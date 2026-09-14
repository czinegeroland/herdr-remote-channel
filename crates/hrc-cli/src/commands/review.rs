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
use hrc_tui::{App, JoinApp, JoinOutcome, Outcome, PendingItem, PendingJoin};
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
