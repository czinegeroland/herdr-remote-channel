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

mod disclosure;
mod joins;
mod onboarding;
mod roster;
mod side_view;
mod writing;

pub use disclosure::*;
pub use joins::*;
pub use onboarding::*;
pub use roster::*;
pub use side_view::*;
pub use writing::*;

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
        let waited = settle(&target);
        let mut delivered = hand_to_herdr(
            &target,
            bodies
                .get(&message_id)
                .map(String::as_str)
                .unwrap_or_default(),
        );
        if let Some(waited) = waited {
            delivered["waitedForIdle"] = json!(waited);
        }
        delivered
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

/// The longest an approved delivery waits for a working agent.
///
/// After this it is delivered anyway. The person approved it to that agent,
/// agents queue input typed mid-turn, and an approval that silently never
/// arrived would be worse than one that arrived during a long turn.
const IDLE_WAIT: Duration = Duration::from_secs(10 * 60);

/// Holds an approved delivery while its destination is mid-turn.
///
/// `None` when there was nothing to wait for: the agent was not working,
/// waiting is turned off, or Herdr could not be asked. Otherwise whether it
/// settled before [`IDLE_WAIT`] ran out.
///
/// The screen has closed by now, so this writes to the pane directly, and
/// says what closing it would mean: the decision is already recorded, and
/// the only thing lost is the hand-over (docs/RESEARCH.md 5.4).
fn settle(target: &str) -> Option<bool> {
    if !super::plugin_config().0.wait_for_idle {
        return None;
    }

    let mut host = crate::herdr_host::Host::connect().ok()?;
    let agent = host
        .destinations()
        .ok()?
        .into_iter()
        .find(|agent| agent.local_target() == target)?;

    if !agent.is_working() {
        return None;
    }

    eprintln!(
        "Approved. {} is working, so this is queued and delivers when it is idle \
         (in {} minutes at the latest).\n\
         Closing this pane now leaves the message approved but not delivered.",
        agent.label(),
        IDLE_WAIT.as_secs() / 60,
    );

    Some(host.wait_until_settled(target, IDLE_WAIT).is_ok())
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

        // A result's commit references are checked against this machine's
        // checkout here, in the trusted process, and shown beside the body
        // rather than inside it.
        let local_checks = super::references::local_checks(&view.kind, body);

        items.push(PendingItem {
            message_id,
            view,
            body: body.to_owned(),
            local_checks,
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

    Ok(hrc_core::time::rfc3339_from(
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

/// One request and answer on the trusted endpoint.
async fn trusted_call(endpoint: &Endpoint, request: TrustedRequest) -> Result<Value> {
    let mut client = connect(endpoint).await?;
    client.call(&request).await.map_err(CliError::from)
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

    /// An administrator and one admitted peer sharing a real repository.
    fn two_member_channel(root: &std::path::Path) -> (Context, String, String) {
        let context = test_context(root);
        let identity = super::super::init(&context).unwrap();

        let remote = root.join("remote.git");
        let status = ProcessCommand::new("git")
            .args(["init", "--bare", "--quiet"])
            .arg(&remote)
            .status()
            .unwrap();
        assert!(status.success());
        super::super::create(&context, remote.to_str().unwrap(), Some("Two members")).unwrap();

        let peer = test_context(&root.join("peer"));
        let peer_identity = super::super::init(&peer).unwrap();
        let invite = super::super::invite_create(&context, "peer", "24h").unwrap();
        let joined = super::super::join(&peer, invite["inviteCode"].as_str().unwrap()).unwrap();
        super::super::admit_join(&context, joined["requestId"].as_str().unwrap()).unwrap();

        (
            context,
            identity["principalKey"].as_str().unwrap().to_owned(),
            peer_identity["principalKey"].as_str().unwrap().to_owned(),
        )
    }

    #[test]
    fn the_local_principal_is_the_principal_and_not_the_device_key() {
        // docs/REFACTOR.md R4. `init` makes two keys and the roster lists
        // principals. The screens read the local principal from `whoami`'s
        // `signingKey`, which is the device's, so nothing ever matched.
        let directory = tempfile::tempdir().unwrap();
        let (context, principal, _peer) = two_member_channel(directory.path());

        let identity = super::super::local_identity(&context).unwrap();
        assert_eq!(identity.principal_id, principal);
        assert_ne!(
            identity.principal_id, identity.device_signing_key,
            "a principal and a device are different keys"
        );
    }

    #[test]
    fn the_membership_screen_recognizes_this_installation() {
        // `is_local` is what lets the screen refuse to remove this
        // installation. It was never true, so that refusal never applied.
        let directory = tempfile::tempdir().unwrap();
        let (context, principal, peer) = two_member_channel(directory.path());

        let rows = member_rows(&context).unwrap();
        let local: Vec<_> = rows.iter().filter(|row| row.is_local).collect();

        assert_eq!(local.len(), 1, "exactly one row is this installation");
        assert_eq!(local[0].principal_id, principal);
        assert!(
            rows.iter()
                .any(|row| row.principal_id == peer && !row.is_local)
        );
    }

    #[test]
    fn this_installation_is_not_offered_as_its_own_recipient() {
        let directory = tempfile::tempdir().unwrap();
        let (context, principal, peer) = two_member_channel(directory.path());

        let recipients: Vec<String> = addressable(&context)
            .unwrap()
            .into_iter()
            .map(|recipient| recipient.principal_id)
            .collect();

        assert_eq!(recipients, vec![peer]);
        assert!(!recipients.contains(&principal));
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
