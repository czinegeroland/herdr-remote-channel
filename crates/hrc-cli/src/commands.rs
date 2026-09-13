//! Implementations of the commands that need no network.
//!
//! Each returns a JSON value. The human renderer formats that same value, so
//! the two output modes cannot drift: there is one source of truth for what
//! a command reports, and `--json` is a formatting choice rather than a
//! separate code path.

use std::time::Duration;

use hrc_core::rpc::{AgentRequest, Broker, ChannelStatus, Request, TrustedRequest, dispatch_agent};
use hrc_core::sync::{PollActivity, poll_interval};
use hrc_crypto::{DeviceSecrets, KeyStore, PassphraseStore};
use hrc_ipc::endpoint::{Endpoint, Interface, prepare_runtime_dir};
use hrc_ipc::serve;
use hrc_storage::Database;
use hrc_transport::{ObjectClass, Transport};
use hrc_transport_git::GitTransport;
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::error::{CliError, Result};
use crate::paths::Paths;

/// Environment variable supplying the key store passphrase.
///
/// Non-interactive automation needs some way to unlock the store. An
/// environment variable is visible to other processes of the same user, so
/// it is an explicit opt-in for automation rather than the recommended path
/// for a person at a terminal.
pub const PASSPHRASE_VARIABLE: &str = "HRC_PASSPHRASE";

/// The name this installation's device keys are stored under.
const DEVICE_KEY_NAME: &str = "device";

/// Everything a command needs from its environment.
///
/// The passphrase is passed in rather than read here, so nothing in this
/// module depends on process-global state. That keeps commands testable
/// without mutating the environment, which is shared by every test in a
/// binary and cannot be changed safely once threads exist.
#[derive(Debug, Clone)]
pub struct Context {
    /// Where local state lives.
    pub paths: Paths,
    /// Key store passphrase, when one was supplied.
    pub passphrase: Option<SecretString>,
}

impl Context {
    /// Builds a context from resolved paths and the environment.
    pub fn from_environment(paths: Paths) -> Self {
        let passphrase = std::env::var(PASSPHRASE_VARIABLE)
            .ok()
            .filter(|value| !value.is_empty())
            .map(SecretString::from);

        Self { paths, passphrase }
    }

    /// Opens the key store, or explains what is missing.
    fn key_store(&self) -> Result<PassphraseStore> {
        let passphrase = self.passphrase.clone().ok_or(CliError::NoPassphrase)?;

        Ok(PassphraseStore::open(self.paths.keys(), passphrase)?)
    }
}

/// `hrc init`: generate this device's keys and local state.
pub fn init(context: &Context) -> Result<Value> {
    let paths = &context.paths;
    paths.ensure()?;
    let store = context.key_store()?;

    if store.contains(DEVICE_KEY_NAME)? {
        // Overwriting would destroy the identity this installation is known
        // by, silently orphaning it from every channel it has joined.
        return Err(CliError::AlreadyInitialized);
    }

    let secrets = DeviceSecrets::generate()?;
    store.save(DEVICE_KEY_NAME, &secrets)?;

    // Creating the database after the keys means a half-finished init leaves
    // no state claiming an identity that does not exist.
    Database::open(paths.database())?;

    Ok(json!({
        "status": "ok",
        "home": paths.home().display().to_string(),
        "signingKey": secrets.signing_key().verifying_key().to_base64url(),
        "encryptionRecipient": secrets.device_identity()?.recipient().to_string(),
    }))
}

/// `hrc whoami`: report this device's public identity.
pub fn whoami(context: &Context) -> Result<Value> {
    let store = context.key_store()?;
    let secrets = store.load(DEVICE_KEY_NAME)?;

    Ok(json!({
        "status": "ok",
        "signingKey": secrets.signing_key().verifying_key().to_base64url(),
        "encryptionRecipient": secrets.device_identity()?.recipient().to_string(),
    }))
}

/// `hrc channels`: list configured channels.
pub fn channels(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;

    let channels: Vec<Value> = database
        .channels()?
        .into_iter()
        .map(|channel| {
            json!({
                "channelId": channel.channel_id,
                "localName": channel.local_name,
                "transport": channel.transport_kind,
                "locator": channel.transport_locator,
                "rosterEpoch": channel.roster_epoch,
                "halted": channel.halted_reason.is_some(),
            })
        })
        .collect();

    Ok(json!({ "status": "ok", "channels": channels }))
}

/// `hrc status`: report the fields PRD section 29 requires.
pub fn status(context: &Context) -> Result<Value> {
    let paths = &context.paths;
    let database = Database::open(paths.database())?;

    let mut channels = Vec::new();
    for channel in database.channels()? {
        let counts = database.channel_counts(&channel.channel_id)?;

        channels.push(json!({
            "channelId": channel.channel_id,
            "localName": channel.local_name,
            "transport": channel.transport_kind,
            "locator": channel.transport_locator,
            "rosterEpoch": channel.roster_epoch,
            "controlSequence": channel.control_sequence,
            "lastFetchedRevision": channel.sync_cursor,
            "outboxPending": counts.outbox_pending,
            "unread": counts.unread,
            "pendingApproval": counts.pending_approval,
            "lastTransportError": counts.last_error,
            "haltedReason": channel.halted_reason,
        }));
    }

    Ok(json!({
        "status": "ok",
        "home": paths.home().display().to_string(),
        "channels": channels,
    }))
}

/// `hrc doctor`: verify the local installation.
///
/// Every check reports its own result rather than the command stopping at
/// the first failure: a user diagnosing a broken installation wants the
/// whole picture, not one symptom at a time.
pub fn doctor(context: &Context) -> Result<Value> {
    let paths = &context.paths;
    let mut checks = Vec::new();
    let mut healthy = true;

    let mut record = |name: &str, ok: bool, detail: String| {
        healthy &= ok;
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };

    record(
        "protocol_version",
        true,
        format!("protocol {}", hrc_protocol::PROTOCOL_VERSION),
    );

    match git_version() {
        Some(version) => record("git", true, version),
        None => record(
            "git",
            false,
            "the git executable was not found on PATH".into(),
        ),
    }

    let home_exists = paths.home().is_dir();
    record(
        "state_directory",
        home_exists,
        paths.home().display().to_string(),
    );

    match Database::open(paths.database()) {
        Ok(database) => match database.integrity_check() {
            Ok(true) => record("database", true, "integrity check passed".into()),
            Ok(false) => record("database", false, "integrity check failed".into()),
            Err(error) => record("database", false, error.to_string()),
        },
        Err(error) => record("database", false, error.to_string()),
    }

    match context.key_store() {
        Ok(store) => match store.contains(DEVICE_KEY_NAME) {
            Ok(true) => record("device_keys", true, "device keys are present".into()),
            Ok(false) => record(
                "device_keys",
                false,
                "no device keys; run `hrc init`".into(),
            ),
            Err(error) => record("device_keys", false, error.to_string()),
        },
        Err(error) => record("device_keys", false, error.to_string()),
    }

    Ok(json!({
        "status": if healthy { "ok" } else { "unhealthy" },
        "healthy": healthy,
        "checks": checks,
    }))
}

/// `hrc audit`: show the local audit log.
pub fn audit(context: &Context, since: Option<&str>) -> Result<Value> {
    let database = Database::open(context.paths.database())?;

    let entries: Vec<Value> = database
        .audit_entries(since)?
        .into_iter()
        .map(|entry| {
            json!({
                "occurredAt": entry.occurred_at,
                "action": entry.action,
                "channelId": entry.channel_id,
                "messageId": entry.message_id,
                "detail": entry.detail,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "entries": entries,
    }))
}

/// `hrc sync --once`: run one synchronization pass for every configured channel.
pub fn sync_once(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let mut channels = Vec::new();

    for channel in database.channels()? {
        channels.push(sync_git_channel(&context.paths, &database, &channel, &now)?);
    }

    Ok(json!({
        "status": "ok",
        "syncedAt": now,
        "channels": channels,
    }))
}

/// `hrc daemon`: stay resident and keep synchronizing in the background.
pub fn daemon_tick(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let mut channels = Vec::new();
    let mut healthy = true;

    for channel in database.channels()? {
        match sync_git_channel(&context.paths, &database, &channel, &now) {
            Ok(value) => channels.push(value),
            Err(error) => {
                healthy = false;
                channels.push(json!({
                    "channelId": channel.channel_id,
                    "transport": channel.transport_kind,
                    "locator": channel.transport_locator,
                    "status": "error",
                    "code": error.code(),
                    "message": error.to_string(),
                }));
            }
        }
    }

    let next_poll = daemon_poll_interval(context)?;
    Ok(json!({
        "status": if healthy { "ok" } else { "degraded" },
        "syncedAt": now,
        "nextPollSeconds": next_poll.as_secs(),
        "channels": channels,
    }))
}

/// Runs the resident daemon loop until the process is stopped.
pub async fn daemon(context: &Context) -> Result<()> {
    let agent_endpoint = daemon_endpoint(&context.paths, Interface::AgentSafe)?;
    let trusted_endpoint = daemon_endpoint(&context.paths, Interface::TrustedHuman)?;
    let agent_context = context.clone();
    let trusted_context = context.clone();
    let loop_context = context.clone();

    tokio::try_join!(
        async {
            serve(&agent_endpoint, move |request: Request| {
                handle_agent_request(&agent_context, request)
            })
            .await
            .map_err(CliError::from)
        },
        async {
            serve(&trusted_endpoint, move |request: TrustedRequest| {
                handle_trusted_request(&trusted_context, request)
            })
            .await
            .map_err(CliError::from)
        },
        daemon_sync_loop(&loop_context),
    )?;

    Ok(())
}

async fn daemon_sync_loop(context: &Context) -> Result<()> {
    loop {
        let delay = daemon_poll_interval(context)?;
        tokio::time::sleep(delay).await;

        if let Err(error) = daemon_tick(context) {
            eprintln!("error: {error}");
        }
    }
}

fn daemon_poll_interval(context: &Context) -> Result<Duration> {
    let database = Database::open(context.paths.database())?;
    let mut next = poll_interval(PollActivity::Background, Duration::ZERO);

    for channel in database.channels()? {
        if channel.transport_kind != "git" {
            continue;
        }

        let git_dir = context.paths.channel_transport(&channel.channel_id);
        if let Some(parent) = git_dir.parent() {
            std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
                action: "create the local transport directory",
                source,
            })?;
        }

        let transport = GitTransport::open(&git_dir, &channel.transport_locator)?;
        let minimum =
            Duration::from_secs(transport.capabilities().min_poll_interval_seconds as u64);
        next = next.min(poll_interval(PollActivity::Background, minimum));
    }

    Ok(next)
}

fn daemon_endpoint(paths: &Paths, interface: Interface) -> Result<Endpoint> {
    let runtime = paths.runtime();
    prepare_runtime_dir(&runtime)?;
    Ok(Endpoint::new(&runtime, interface)?)
}

fn handle_agent_request(context: &Context, request: Request) -> Value {
    match request {
        request @ Request::Agent(AgentRequest::Inbox { .. })
        | request @ Request::Agent(AgentRequest::ShowApproved { .. })
        | request @ Request::Agent(AgentRequest::Draft { .. })
        | request @ Request::Agent(AgentRequest::Wait { .. }) => json!({
            "status": "error",
            "code": "unimplemented",
            "message": format!(
                "the `{}` agent-safe daemon method is not implemented yet",
                request.method()
            ),
        }),
        other => {
            let mut broker = match DaemonBroker::new(context) {
                Ok(broker) => broker,
                Err(error) => {
                    return json!({
                        "status": "error",
                        "code": error.code(),
                        "message": error.to_string(),
                    });
                }
            };

            match dispatch_agent(&mut broker, other) {
                Ok(response) => json!({
                    "status": "ok",
                    "response": response,
                }),
                Err(error) => json!({
                    "status": "error",
                    "code": core_error_code(&error),
                    "message": error.to_string(),
                }),
            }
        }
    }
}

fn handle_trusted_request(_context: &Context, request: TrustedRequest) -> Value {
    json!({
        "status": "error",
        "code": "unimplemented",
        "message": format!(
            "the trusted daemon method `{}` is not implemented yet",
            request.method()
        ),
    })
}

fn core_error_code(error: &hrc_core::CoreError) -> &'static str {
    match error {
        hrc_core::CoreError::AuthorizationRequired { .. } => "authorization_required",
        _ => "core_error",
    }
}

struct DaemonBroker {
    channel_status: Vec<ChannelStatus>,
    channel_names: Vec<String>,
    principal_id: String,
    device_id: String,
    checks: Vec<(String, bool)>,
    audit: Vec<String>,
}

impl DaemonBroker {
    fn new(context: &Context) -> Result<Self> {
        let database = Database::open(context.paths.database())?;
        let channel_records = database.channels()?;
        let channel_status = channel_records
            .iter()
            .map(|channel| {
                let counts = database.channel_counts(&channel.channel_id)?;
                Ok(ChannelStatus {
                    local_name: channel.local_name.clone(),
                    roster_epoch: channel.roster_epoch,
                    pending: counts.pending_approval as usize,
                    halted: channel.halted_reason.is_some(),
                })
            })
            .collect::<Result<Vec<_>>>()?;
        let channel_names = channel_records
            .iter()
            .map(|channel| channel.local_name.clone())
            .collect();

        let store = context.key_store()?;
        let secrets = store.load(DEVICE_KEY_NAME)?;
        let principal_id = secrets.signing_key().verifying_key().to_base64url();
        let device_id = secrets.device_identity()?.recipient().to_string();

        let checks = doctor(context)?
            .get("checks")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|check| {
                Some((
                    check.get("check")?.as_str()?.to_owned(),
                    check.get("ok")?.as_bool()?,
                ))
            })
            .collect();

        let audit = database
            .audit_entries(None)?
            .into_iter()
            .map(|entry| format_audit_entry(&entry))
            .collect();

        Ok(Self {
            channel_status,
            channel_names,
            principal_id,
            device_id,
            checks,
            audit,
        })
    }
}

impl Broker for DaemonBroker {
    fn channel_status(&self) -> Vec<ChannelStatus> {
        self.channel_status.clone()
    }

    fn channel_names(&self) -> Vec<String> {
        self.channel_names.clone()
    }

    fn local_identity(&self) -> (String, String) {
        (self.principal_id.clone(), self.device_id.clone())
    }

    fn inbox(&self, _pending_only: bool) -> Vec<hrc_core::gate::AgentView> {
        Vec::new()
    }

    fn approved_content(&self, _message_id: &str) -> Option<String> {
        None
    }

    fn checks(&self) -> Vec<(String, bool)> {
        self.checks.clone()
    }

    fn audit(&self) -> Vec<String> {
        self.audit.clone()
    }

    fn record_draft(&mut self, _recipient: &str, _text: &str, _endpoint: Option<&str>) -> String {
        "unavailable".into()
    }

    fn observe(&self, _message_id: &str, _until: &str) -> Option<String> {
        None
    }

    fn pending(&self, _message_id: &str) -> Option<&hrc_core::message::QuarantinedMessage> {
        None
    }

    fn pending_body(&self, _message_id: &str) -> Option<String> {
        None
    }

    fn channel_local_name(&self, _message_id: &str) -> String {
        "local channel".into()
    }

    fn local_user(&self) -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local human".into())
    }

    fn record_decision(
        &mut self,
        _message_id: &str,
        _decision: &hrc_core::gate::Decision,
    ) -> hrc_core::Result<()> {
        Ok(())
    }

    fn apply_trusted(&mut self, _request: &TrustedRequest) -> hrc_core::Result<()> {
        Ok(())
    }
}

fn format_audit_entry(entry: &hrc_storage::AuditEntry) -> String {
    let mut line = format!("{} {}", entry.occurred_at, entry.action);
    if let Some(channel_id) = &entry.channel_id {
        line.push_str(&format!(" channel={channel_id}"));
    }
    if let Some(message_id) = &entry.message_id {
        line.push_str(&format!(" message={message_id}"));
    }
    if let Some(detail) = &entry.detail {
        line.push_str(&format!(" detail={detail}"));
    }
    line
}

fn sync_git_channel(
    paths: &Paths,
    database: &Database,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<Value> {
    if channel.transport_kind != "git" {
        return Err(CliError::Io {
            action: "open the configured transport",
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported transport {}", channel.transport_kind),
            ),
        });
    }

    let git_dir = paths.channel_transport(&channel.channel_id);
    if let Some(parent) = git_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
            action: "create the local transport directory",
            source,
        })?;
    }

    let mut transport = GitTransport::open(&git_dir, &channel.transport_locator)?;
    let remote_head = transport.remote_head()?;
    let local_missing = matches!(
        transport.open_group(),
        Err(hrc_transport::TransportError::NoSuchGroup)
    );
    let remote_changed = remote_head != channel.sync_cursor;
    let recovered = database.recover_reservations(&channel.channel_id, now)?;

    if remote_changed || (remote_head.is_some() && local_missing) {
        transport.sync_from_remote()?;
    }

    let fetch = if remote_changed {
        Some(hrc_core::sync::fetch_once(
            &transport,
            database,
            &channel.channel_id,
            100,
            now,
        )?)
    } else {
        None
    };

    let published = database
        .pending_outgoing_records(&channel.channel_id)?
        .into_iter()
        .map(|record| {
            hrc_core::sync::publish_one(
                &mut transport,
                database,
                &channel.channel_id,
                &record.message_id,
                outgoing_message_object(&record.message_id, &record.created_at, record.ciphertext),
                3,
                now,
            )
        })
        .collect::<std::result::Result<Vec<_>, _>>()?;

    let published_messages = published
        .iter()
        .flat_map(|outcome| outcome.published.iter().cloned())
        .collect::<Vec<_>>();
    let deferred_messages = published
        .iter()
        .flat_map(|outcome| outcome.deferred.iter().cloned())
        .collect::<Vec<_>>();
    let conflicts = published
        .iter()
        .map(|outcome| outcome.conflicts)
        .sum::<u32>();

    Ok(json!({
        "channelId": channel.channel_id,
        "transport": channel.transport_kind,
        "locator": channel.transport_locator,
        "remoteChanged": remote_changed,
        "remoteHead": remote_head,
        "recoveredReservations": recovered,
        "publishedMessages": published_messages,
        "deferredMessages": deferred_messages,
        "publishConflicts": conflicts,
        "fetchedPublications": fetch.as_ref().map(|outcome| outcome.publications).unwrap_or(0),
        "fetchedControlObjects": fetch.as_ref().map(|outcome| outcome.control_objects).unwrap_or(0),
        "fetchedMessageObjects": fetch.as_ref().map(|outcome| outcome.message_objects).unwrap_or(0),
        "cursor": fetch.and_then(|outcome| outcome.cursor),
    }))
}

fn outgoing_message_object(
    message_id: &str,
    created_at: &str,
    ciphertext: Vec<u8>,
) -> hrc_transport::PublishObject {
    let year = created_at.get(0..4).unwrap_or("0000");
    let month = created_at.get(5..7).unwrap_or("00");

    hrc_transport::PublishObject {
        name: format!("messages/{year}/{month}/{message_id}.age"),
        class: ObjectClass::Message,
        bytes: ciphertext,
    }
}

/// The installed Git version, if Git is on the path.
fn git_version() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}

#[cfg(test)]
mod tests;
