//! Implementations of the commands that need no network.
//!
//! Each returns a JSON value. The human renderer formats that same value, so
//! the two output modes cannot drift: there is one source of truth for what
//! a command reports, and `--json` is a formatting choice rather than a
//! separate code path.

use hrc_crypto::{DeviceSecrets, KeyStore, PassphraseStore};
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
#[derive(Debug)]
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
