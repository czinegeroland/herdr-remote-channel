//! Implementations of the commands that need no network.
//!
//! Each returns a JSON value. The human renderer formats that same value, so
//! the two output modes cannot drift: there is one source of truth for what
//! a command reports, and `--json` is a formatting choice rather than a
//! separate code path.

use std::collections::HashMap;
use std::path::{Component, Path};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hrc_core::rpc::{AgentRequest, Broker, ChannelStatus, Request, TrustedRequest, dispatch_agent};
use hrc_core::sync::{PollActivity, poll_interval};
use hrc_core::{ContextPackage, ExcludedPath, Roster};
use hrc_crypto::enrollment::Invite;
use hrc_crypto::{DeviceSecrets, InviteSecret, KeyStore, PassphraseStore, PrincipalSecrets};
use hrc_ipc::endpoint::{Endpoint, Interface, prepare_runtime_dir};
use hrc_ipc::serve;
use hrc_protocol::canonical;
use hrc_protocol::control::{
    ControlEntryPayload, ControlOperation, GenesisPayload, PrincipalMaterial, TransportLocator,
};
use hrc_protocol::domain;
use hrc_protocol::identity::{DeviceCertificatePayload, DeviceDescriptor};
use hrc_protocol::message::MessageEnvelope;
use hrc_protocol::signed::{SignedObject, Signer};
use hrc_storage::{Database, PendingOutgoing};
use hrc_transport::{ObjectClass, PublishObject, Revision, Transport};
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

/// The name this installation's principal key is stored under.
///
/// Separate from the device key because they answer for different things: a
/// device key signs messages from one machine, while a principal key vouches
/// for which devices belong to the person (PRD requirement HRC-CH-006).
const PRINCIPAL_KEY_NAME: &str = "principal";

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
    let principal = PrincipalSecrets::generate()?;

    // Both keys or neither. An installation with a device key and no
    // principal key could sign messages but could not prove the device was
    // its own, which is a state nothing else knows how to recover from.
    store.save(PRINCIPAL_KEY_NAME, &principal)?;
    store.save(DEVICE_KEY_NAME, &secrets)?;

    // Creating the database after the keys means a half-finished init leaves
    // no state claiming an identity that does not exist.
    Database::open(paths.database())?;

    Ok(json!({
        "status": "ok",
        "home": paths.home().display().to_string(),
        "principalKey": principal.signing_key().verifying_key().to_base64url(),
        "signingKey": secrets.signing_key().verifying_key().to_base64url(),
        "encryptionRecipient": secrets.device_identity()?.recipient().to_string(),
    }))
}

/// `hrc create`: create a channel and publish its genesis object.
///
/// Genesis is the channel's identity: the channel ID is the hash of the
/// genesis payload, so everything below has to be settled before the channel
/// exists at all. The order matters — the device certificate is signed by the
/// principal key, genesis carries that certificate and is signed by the
/// device key, and only then is the channel ID derivable.
///
/// Publication comes before the local record. A channel registered locally
/// but never published would be one this installation believes in and nobody
/// else can see.
/// Expands the `owner/name` shorthand the CLI asks for into a URL git can use.
///
/// Section 22.4 and `--help` both spell the argument `owner/name`, and that
/// form did not work: nothing expanded it, so the locator reached git as
/// written and git resolved it as a relative path on the local filesystem.
/// The first real channel created from the documented form failed with a
/// path error, and only a full URL succeeded.
///
/// Anything already carrying a scheme, an SCP-style `host:path`, or a
/// filesystem path is left exactly as given: a local bare repository is a
/// legitimate transport and must not be rewritten into a GitHub URL. Only the
/// unambiguous two-segment shorthand is expanded — the same shape npm reads as
/// `owner/repo`, which this repository has already been bitten by once
/// (decision DEC-060).
fn canonical_repository(repo: &str) -> String {
    let trimmed = repo.trim();

    let looks_like_a_path = trimmed.starts_with('/')
        || trimmed.starts_with('.')
        || trimmed.starts_with('~')
        || trimmed.starts_with('\\')
        || trimmed.as_bytes().get(1).is_some_and(|byte| *byte == b':');

    if trimmed.contains("://") || trimmed.contains('@') || looks_like_a_path {
        return trimmed.to_owned();
    }

    let mut segments = trimmed.split('/');
    let (Some(owner), Some(name), None) = (segments.next(), segments.next(), segments.next())
    else {
        return trimmed.to_owned();
    };

    if owner.is_empty() || name.is_empty() {
        return trimmed.to_owned();
    }

    let name = name.strip_suffix(".git").unwrap_or(name);
    format!("https://github.com/{owner}/{name}.git")
}

pub fn create(context: &Context, repo: &str, local_name: Option<&str>) -> Result<Value> {
    let repo = &canonical_repository(repo);
    let paths = &context.paths;
    paths.ensure()?;

    let store = context.key_store()?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;

    let database = Database::open(paths.database())?;
    let now = database.utc_now()?;

    let principal_key = principal.signing_key();
    let principal_id = principal_key.verifying_key().to_base64url();
    let device_key = device.signing_key();

    let descriptor = DeviceDescriptor {
        version: hrc_protocol::PROTOCOL_VERSION,
        principal_id: principal_id.clone(),
        encryption_recipient: device.device_identity()?.recipient().to_string(),
        signing_key: device_key.verifying_key().to_base64url(),
        created_at: now.clone(),
        expires_at: None,
        nonce: canonical::encode_base64url(&random_nonce()?),
    };

    let certificate_payload = DeviceCertificatePayload::new(descriptor)?;
    let device_id = certificate_payload.device_id.clone();
    let signer = Signer {
        principal_id: principal_id.clone(),
        device_id: device_id.clone(),
    };

    let certificate = principal_key.sign_object(
        domain::DEVICE_CERTIFICATE,
        signer.clone(),
        certificate_payload,
    )?;

    let genesis_payload = GenesisPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        created_at: now.clone(),
        initial_admin: PrincipalMaterial {
            principal_id: principal_id.clone(),
            principal_signing_key: principal_id.clone(),
            devices: vec![certificate],
        },
        transport: TransportLocator {
            kind: "git".into(),
            locator: repo.to_owned(),
        },
        policy: serde_json::json!({}),
    };

    let channel_id = genesis_payload.channel_id()?;
    let genesis = device_key.sign_object(domain::GENESIS, signer, genesis_payload)?;

    // Verifying our own genesis before publishing it. A channel whose
    // identity object does not validate would be unusable by everyone
    // including its creator, and the cost of finding out now is nothing.
    Roster::from_genesis(&genesis)?;

    if database.channel(&channel_id)?.is_some() {
        return Err(CliError::ChannelExists {
            channel_id: channel_id.clone(),
        });
    }

    let mut transport = GitTransport::open(paths.channel_transport(&channel_id), repo)?;
    transport.create_group(vec![
        PublishObject {
            name: "protocol.json".into(),
            class: ObjectClass::Protocol,
            bytes: canonical::to_canonical_bytes(&serde_json::json!({
                "version": hrc_protocol::PROTOCOL_VERSION,
            }))?,
        },
        PublishObject {
            name: format!("control/log/00000000-{}.json", &channel_id[..16]),
            class: ObjectClass::Control,
            bytes: canonical::to_canonical_bytes(&genesis)?,
        },
    ])?;

    database.insert_channel(&channel_id, "git", repo, local_name.unwrap_or(repo), &now)?;
    database.append_audit(
        Some(&channel_id),
        None,
        "create_channel",
        Some(&channel_id),
        Some(repo),
        &now,
    )?;

    Ok(json!({
        "status": "ok",
        "channelId": channel_id,
        "repo": repo,
        "principalId": principal_id,
        "deviceId": device_id,
    }))
}

/// Replays a channel's published control log into a roster.
///
/// The control log is the authority on membership, so it is read from the
/// transport rather than from local state: a local cache could be stale or
/// edited, and the point of the chain is that every participant can derive
/// the same answer from the same published objects.
fn load_roster(transport: &GitTransport, channel_id: &str) -> Result<(Roster, Revision)> {
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    let mut cursor = None;
    let mut head = None;

    loop {
        let page = transport.fetch(cursor.as_deref(), 100)?;

        for publication in &page.publications {
            head = Some(publication.revision.clone());

            for object in &publication.objects {
                if object.class == ObjectClass::Control {
                    let bytes = transport.get_object(&object.name, &object.sha256)?;
                    entries.push((object.name.clone(), bytes));
                }
            }
        }

        if !page.more {
            break;
        }
        cursor = page.cursor;
    }

    // Names carry the control sequence as a zero-padded prefix, so sorting
    // by name is sorting by position in the chain.
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let mut objects = entries.into_iter();
    let (_, genesis_bytes) = objects
        .next()
        .ok_or_else(|| CliError::ChannelNotPublished {
            channel_id: channel_id.to_owned(),
        })?;

    let genesis: SignedObject<GenesisPayload> =
        canonical::from_json_str(std::str::from_utf8(&genesis_bytes).map_err(|_| {
            CliError::ChannelNotPublished {
                channel_id: channel_id.to_owned(),
            }
        })?)?;

    let mut roster = Roster::from_genesis(&genesis)?;

    for (_, bytes) in objects {
        let entry: SignedObject<ControlEntryPayload> =
            canonical::from_json_str(std::str::from_utf8(&bytes).map_err(|_| {
                CliError::ChannelNotPublished {
                    channel_id: channel_id.to_owned(),
                }
            })?)?;
        roster.apply(&entry)?;
    }

    let head = head.ok_or_else(|| CliError::ChannelNotPublished {
        channel_id: channel_id.to_owned(),
    })?;

    Ok((roster, head))
}

/// Signs and publishes one control entry, advancing the chain.
fn publish_control(
    transport: &mut GitTransport,
    roster: &Roster,
    revision: Revision,
    device_key: &hrc_crypto::SigningKey,
    signer: Signer,
    operation: ControlOperation,
    now: &str,
) -> Result<String> {
    let epoch = if operation.advances_epoch() {
        roster.epoch() + 1
    } else {
        roster.epoch()
    };

    let payload = ControlEntryPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: roster.channel_id().to_owned(),
        sequence: roster.sequence() + 1,
        previous_hash: roster.head_hash().to_owned(),
        epoch,
        created_at: now.to_owned(),
        operation,
    };
    payload.validate_shape()?;

    let name = format!(
        "control/log/{:08}-{}.json",
        payload.sequence,
        &payload.entry_hash()?[..16]
    );
    let entry = device_key.sign_object(domain::CONTROL, signer, payload)?;

    // A control-only publication, as PRD section 16.3 requires: a commit
    // that mixed control and data would leave the resulting epoch ambiguous
    // about which objects it applies to.
    transport.publish(hrc_transport::PublishRequest {
        expected_revision: Some(revision),
        class: hrc_transport::PublicationClass::Control,
        objects: vec![PublishObject {
            name: name.clone(),
            class: ObjectClass::Control,
            bytes: canonical::to_canonical_bytes(&entry)?,
        }],
    })?;

    Ok(name)
}

/// The single channel this installation is configured for, if there is one.
pub(crate) fn only_channel(database: &Database) -> Result<hrc_storage::ChannelRecord> {
    let mut channels = database.channels()?;

    match channels.len() {
        1 => Ok(channels.remove(0)),
        0 => Err(CliError::NoChannel),
        _ => Err(CliError::AmbiguousChannel),
    }
}

/// `hrc invite create`: authorize one enrollment.
///
/// The invite code is returned to the caller for the human to hand over. It
/// is deliberately *not* available under `--json`: the whole output is a
/// secret, and machine-readable output is the form most likely to end up in
/// a transcript, a log, or an agent's context. See decision DEC-047.
///
/// What is published is only the invite's identifier and expiry. The secret
/// never reaches the channel, because anyone who could read the repository
/// could then enroll.
pub fn invite_create(context: &Context, intended_for: &str, expires_in: &str) -> Result<Value> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let expires_at = expiry_from(&now, expires_in)?;
    let channel = only_channel(&database)?;

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, revision) = load_roster(&transport, &channel.channel_id)?;

    let invite_id = canonical::encode_base64url(&random_nonce()?);
    let invite = Invite::generate(
        &channel.transport_locator,
        &channel.channel_id,
        principal.signing_key().verifying_key().to_base64url(),
        &invite_id,
        &expires_at,
    )?;

    let signer = Signer {
        principal_id: principal.signing_key().verifying_key().to_base64url(),
        device_id: local_device_id(&roster, &device)?,
    };

    publish_control(
        &mut transport,
        &roster,
        revision,
        &device.signing_key(),
        signer,
        ControlOperation::CreateInvite {
            invite_id: invite_id.clone(),
            expires_at: expires_at.clone(),
        },
        &now,
    )?;

    // Retained by its issuer, protected at rest by the same passphrase as a
    // signing key. Verifying a join proof later needs this exact value, and
    // holding it is a different thing from publishing it (DEC-047).
    store.save(
        &invite_store_name(&invite_id),
        &InviteSecret::new(invite.to_code()?),
    )?;

    database.record_invite(
        &channel.channel_id,
        &invite_id,
        intended_for,
        &expires_at,
        &now,
    )?;
    database.append_audit(
        Some(&channel.channel_id),
        None,
        "create_invite",
        None,
        Some(&invite_id),
        &now,
    )?;

    let join_link = hrc_herdr::link::join_link(&channel.transport_locator);
    let join_link = hrc_herdr::link::locator_from(&join_link)
        .is_some()
        .then_some(join_link);

    Ok(json!({
        "status": "ok",
        "inviteId": invite_id,
        "expiresAt": expires_at,
        "intendedFor": intended_for,
        // The one place this value appears. Hand it over through a channel
        // the user chooses; it works once and then it is spent.
        "inviteCode": invite.to_code()?,
        // A link that opens the join step for whoever clicks it in Herdr.
        // It carries the public locator and never the code: a URL lands in
        // scrollback, clipboard managers and whatever it was pasted into, and
        // a click is not the moment to decide whether a link is genuine.
        // Only a web locator makes a clickable link, so a channel on a local
        // path has none.
        "joinLink": join_link,
    }))
}

/// `hrc invite list`: outstanding invites, without their secrets.
pub fn invite_list(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let invites: Vec<Value> = database
        .invites(&channel.channel_id)?
        .into_iter()
        .map(|invite| {
            json!({
                "inviteId": invite.invite_id,
                "intendedFor": invite.intended_for,
                "expiresAt": invite.expires_at,
                "state": invite.state,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "invites": invites,
    }))
}

/// `hrc invite revoke`: withdraw an unused invite.
pub fn invite_revoke(context: &Context, invite_id: &str) -> Result<Value> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, revision) = load_roster(&transport, &channel.channel_id)?;

    let signer = Signer {
        principal_id: principal.signing_key().verifying_key().to_base64url(),
        device_id: local_device_id(&roster, &device)?,
    };

    publish_control(
        &mut transport,
        &roster,
        revision,
        &device.signing_key(),
        signer,
        ControlOperation::RevokeInvite {
            invite_id: invite_id.to_owned(),
        },
        &now,
    )?;

    // A withdrawn invite can admit nobody, so its secret has no remaining
    // purpose and keeping it would be keeping a bearer credential for
    // nothing.
    store.delete(&invite_store_name(invite_id))?;
    database.set_invite_state(&channel.channel_id, invite_id, "revoked")?;
    database.append_audit(
        Some(&channel.channel_id),
        None,
        "revoke_invite",
        None,
        Some(invite_id),
        &now,
    )?;

    Ok(json!({
        "status": "ok",
        "inviteId": invite_id,
        "state": "revoked",
    }))
}

/// `hrc join <invite-code>`: ask to be admitted to a channel.
///
/// The joiner has no channel yet, so everything is derived from the invite:
/// where the repository is, which channel it holds, and the one-time secret
/// that authorizes the request. The repository is cloned and its genesis
/// replayed before anything is sent, because the administrator's devices are
/// the only ones the request may be encrypted to and the roster is where that
/// set comes from.
///
/// The safety phrase is returned for the human to read aloud. It is not a
/// confirmation: nothing here can check it, and an implementation that could
/// would be the thing an attacker compromises.
pub fn join(context: &Context, invite_code: &str) -> Result<Value> {
    let paths = &context.paths;
    paths.ensure()?;

    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let invite = Invite::from_code(invite_code)?;
    let database = Database::open(paths.database())?;
    let now = database.utc_now()?;

    if invite.is_expired_at(&now) {
        return Err(CliError::InviteExpired {
            expires_at: invite.expires_at.clone(),
        });
    }

    let transport =
        GitTransport::open(paths.channel_transport(&invite.channel_id), &invite.locator)?;
    transport.sync_from_remote()?;

    let (roster, _) = load_roster(&transport, &invite.channel_id)?;

    // The invite names a channel; the repository contains one. If they
    // disagree, the invite is for somewhere else and nothing about this
    // repository is what the joiner was told it is.
    if roster.channel_id() != invite.channel_id {
        return Err(CliError::InviteChannelMismatch {
            expected: invite.channel_id.clone(),
            found: roster.channel_id().to_owned(),
        });
    }

    let principal_key = principal.signing_key();
    let principal_id = principal_key.verifying_key().to_base64url();
    let device_key = device.signing_key();

    let descriptor = DeviceDescriptor {
        version: hrc_protocol::PROTOCOL_VERSION,
        principal_id: principal_id.clone(),
        encryption_recipient: device.device_identity()?.recipient().to_string(),
        signing_key: device_key.verifying_key().to_base64url(),
        created_at: now.clone(),
        expires_at: None,
        nonce: canonical::encode_base64url(&random_nonce()?),
    };

    let certificate_payload = DeviceCertificatePayload::new(descriptor)?;
    let certificate = principal_key.sign_object(
        domain::DEVICE_CERTIFICATE,
        Signer {
            principal_id: principal_id.clone(),
            device_id: certificate_payload.device_id.clone(),
        },
        certificate_payload,
    )?;

    let phrase =
        hrc_core::enrollment::joiner_safety_phrase(&roster, &invite, &principal_id, &certificate)?;

    let request = hrc_core::enrollment::request_join(
        &roster,
        &invite,
        &principal_key,
        &principal_id,
        certificate,
        &now,
    )?;

    let request_id = canonical::encode_base64url(&random_nonce()?);
    let mut transport = transport;
    let revision =
        transport
            .open_group()?
            .revision
            .ok_or_else(|| CliError::ChannelNotPublished {
                channel_id: invite.channel_id.clone(),
            })?;

    transport.publish(hrc_transport::PublishRequest {
        expected_revision: Some(revision),
        class: hrc_transport::PublicationClass::Data,
        objects: vec![PublishObject {
            name: format!("joins/{}/{request_id}.age", invite.invite_id),
            class: ObjectClass::Join,
            bytes: request,
        }],
    })?;

    // Registered locally as soon as the request is out, so the joiner can
    // see the channel they are waiting on rather than having to remember it.
    if database.channel(&invite.channel_id)?.is_none() {
        database.insert_channel(
            &invite.channel_id,
            "git",
            &invite.locator,
            &invite.locator,
            &now,
        )?;
    }

    database.append_audit(
        Some(&invite.channel_id),
        None,
        "request_join",
        None,
        Some(&request_id),
        &now,
    )?;

    Ok(json!({
        "status": "ok",
        "channelId": invite.channel_id,
        "requestId": request_id,
        "principalId": principal_id,
        // Read this aloud to the administrator and compare it. Nothing on
        // either machine can confirm it for you.
        "safetyPhrase": phrase.to_string(),
        "wordlist": phrase.wordlist,
    }))
}

/// `hrc join pending`: join requests awaiting this administrator.
///
/// Every request is validated before it is listed, so the list contains
/// proposals rather than claims: a request with a bad proof, a certificate
/// signed by someone else, or an invite this log never opened does not appear
/// at all. Listing it with a warning would put a decision in front of a human
/// that the machine had already answered.
pub fn join_pending(context: &Context) -> Result<Value> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;

    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;

    let (roster, _) = load_roster(&transport, &channel.channel_id)?;
    let identity = device.device_identity()?;

    let mut pending = Vec::new();
    let mut cursor = None;

    loop {
        let page = transport.fetch(cursor.as_deref(), 100)?;

        for publication in &page.publications {
            for object in &publication.objects {
                if object.class != ObjectClass::Join {
                    continue;
                }

                let Some(invite_id) = object
                    .name
                    .strip_prefix("joins/")
                    .and_then(|rest| rest.split('/').next())
                else {
                    continue;
                };

                let Some(invite) = local_invite(&store, &database, &channel.channel_id, invite_id)?
                else {
                    // An invite this installation did not issue. The secret
                    // is not here, so the proof cannot be checked, and an
                    // unverifiable request is not something to show a human.
                    continue;
                };

                let ciphertext = transport.get_object(&object.name, &object.sha256)?;

                if let Ok(review) = hrc_core::enrollment::review_join(
                    &roster,
                    &invite,
                    &identity,
                    &ciphertext,
                    &now,
                ) {
                    pending.push(json!({
                        "requestId": object
                            .name
                            .rsplit('/')
                            .next()
                            .and_then(|file| file.strip_suffix(".age"))
                            .unwrap_or_default(),
                        "inviteId": review.invite_id,
                        "principalId": review.principal_id,
                        "deviceId": review.device_id(),
                        "createdAt": review.created_at,
                        "safetyPhrase": review.safety_phrase.to_string(),
                        "wordlist": review.safety_phrase.wordlist,
                    }));
                }
            }
        }

        if !page.more {
            break;
        }
        cursor = page.cursor;
    }

    Ok(json!({
        "status": "ok",
        "channelId": channel.channel_id,
        "pending": pending,
    }))
}

/// The invite this installation issued under `invite_id`, if it still holds it.
///
/// `None` means the invite is not one this installation issued, or its secret
/// has already been released because it was spent or withdrawn. Either way
/// the proof on a request naming it cannot be checked, and an unverifiable
/// request is not something to put in front of a human.
fn local_invite(
    store: &PassphraseStore,
    database: &Database,
    channel_id: &str,
    invite_id: &str,
) -> Result<Option<Invite>> {
    let known = database
        .invites(channel_id)?
        .into_iter()
        .any(|invite| invite.invite_id == invite_id && invite.state == "open");

    if !known {
        return Ok(None);
    }

    let name = invite_store_name(invite_id);
    if !store.contains(&name)? {
        return Ok(None);
    }

    let secret: InviteSecret = store.load(&name)?;
    Ok(Some(Invite::from_code(secret.code())?))
}

/// Publishes one membership change as a control entry.
///
/// Reached only from the trusted interface: removing a member and revoking a
/// device are both on the section 22.7 list, so this is never called from an
/// agent-safe path.
fn publish_membership_change(context: &Context, operation: ControlOperation) -> Result<String> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, revision) = load_roster(&transport, &channel.channel_id)?;

    let action = operation.name();
    let signer = Signer {
        principal_id: principal.signing_key().verifying_key().to_base64url(),
        device_id: local_device_id(&roster, &device)?,
    };

    let name = publish_control(
        &mut transport,
        &roster,
        revision,
        &device.signing_key(),
        signer,
        operation,
        &now,
    )?;

    database.append_audit(
        Some(&channel.channel_id),
        None,
        action,
        None,
        Some(&name),
        &now,
    )?;

    Ok(name)
}

/// Admits a pending joiner, on the administrator's decision.
///
/// The request is validated again here rather than trusted from the listing:
/// the channel may have moved between the human reading a safety phrase and
/// answering, and the entry that gets published has to be built from what is
/// true now.
fn admit_join(context: &Context, request_id: &str) -> Result<()> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, revision) = load_roster(&transport, &channel.channel_id)?;
    let identity = device.device_identity()?;

    let mut cursor = None;
    let mut admitted = None;

    'outer: loop {
        let page = transport.fetch(cursor.as_deref(), 100)?;

        for publication in &page.publications {
            for object in &publication.objects {
                if object.class != ObjectClass::Join
                    || !object.name.ends_with(&format!("/{request_id}.age"))
                {
                    continue;
                }

                let Some(invite_id) = object
                    .name
                    .strip_prefix("joins/")
                    .and_then(|rest| rest.split('/').next())
                else {
                    continue;
                };

                let Some(invite) = local_invite(&store, &database, &channel.channel_id, invite_id)?
                else {
                    continue;
                };

                let ciphertext = transport.get_object(&object.name, &object.sha256)?;
                admitted = Some((
                    hrc_core::enrollment::review_join(
                        &roster,
                        &invite,
                        &identity,
                        &ciphertext,
                        &now,
                    )?,
                    invite_id.to_owned(),
                ));
                break 'outer;
            }
        }

        if !page.more {
            break;
        }
        cursor = page.cursor;
    }

    let (pending, invite_id) = admitted.ok_or_else(|| CliError::NoSuchMessage {
        message_id: request_id.to_owned(),
    })?;

    let entry = hrc_core::enrollment::admit(
        &roster,
        &device.signing_key(),
        Signer {
            principal_id: principal.signing_key().verifying_key().to_base64url(),
            device_id: local_device_id(&roster, &device)?,
        },
        &pending,
        &now,
    )?;

    transport.publish(hrc_transport::PublishRequest {
        expected_revision: Some(revision),
        class: hrc_transport::PublicationClass::Control,
        objects: vec![PublishObject {
            name: format!(
                "control/log/{:08}-{}.json",
                entry.payload.sequence,
                &entry.payload.entry_hash()?[..16]
            ),
            class: ObjectClass::Control,
            bytes: canonical::to_canonical_bytes(&entry)?,
        }],
    })?;

    // The invite is spent, so its secret has no further purpose.
    store.delete(&invite_store_name(&invite_id))?;
    database.set_invite_state(&channel.channel_id, &invite_id, "consumed")?;
    database.append_audit(
        Some(&channel.channel_id),
        None,
        "approve_join",
        None,
        Some(&pending.principal_id),
        &now,
    )?;

    Ok(())
}

/// `hrc members`: who is in the channel, from the published control log.
pub fn members(context: &Context) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let members: Vec<Value> = roster
        .members()
        .map(|member| {
            json!({
                "principalId": member.principal_id,
                "administrator": member.is_administrator,
                "active": member.is_active,
                "devices": roster
                    .devices()
                    .filter(|device| device.principal_id == member.principal_id)
                    .map(|device| json!({
                        "deviceId": device.device_id,
                        "active": device.status == hrc_core::DeviceStatus::Active,
                        "addedInEpoch": device.added_in_epoch,
                    }))
                    .collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "channelId": channel.channel_id,
        "rosterEpoch": roster.epoch(),
        "members": members,
    }))
}

/// `hrc device list`: this principal's devices.
pub fn device_list(context: &Context) -> Result<Value> {
    let store = context.key_store()?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;
    let principal_id = principal.signing_key().verifying_key().to_base64url();

    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let devices: Vec<Value> = roster
        .devices()
        .filter(|device| device.principal_id == principal_id)
        .map(|device| {
            json!({
                "deviceId": device.device_id,
                "active": device.status == hrc_core::DeviceStatus::Active,
                "addedInEpoch": device.added_in_epoch,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "principalId": principal_id,
        "devices": devices,
    }))
}

/// Opens a fresh transport and device identity for the receive pass.
///
/// Separate from the synchronization pass because receiving needs the key
/// store, and a channel can be synchronized by a process that has no
/// passphrase. A locked installation can still validate and advance public
/// state; queued ciphertext that became stale stays deferred until the key
/// store can be unlocked.
fn receive_for(
    context: &Context,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<Vec<String>> {
    let identity = match context.passphrase.clone() {
        Some(passphrase) => {
            let store = PassphraseStore::open(context.paths.keys(), passphrase)?;
            if store.contains(DEVICE_KEY_NAME)? {
                let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
                Some(device.device_identity()?)
            } else {
                None
            }
        }
        None => None,
    };

    let transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;

    let mut database = Database::open(context.paths.database())?;
    let received = receive_messages(&transport, &mut database, channel, identity.as_ref(), now)?;

    Ok(received)
}

/// Tells each sender that their messages arrived (PRD section 18.2).
///
/// Pending deliveries are recovered from durable inbox and audit records,
/// then batched within the protocol's limit. A failed publication must not
/// lose the obligation to report an already accepted message.
///
/// Only `delivered` is reported here. `read` is a different claim — that a
/// human opened it — and whether to make it the default is still open
/// question OQ-007, so nothing here decides it.
///
/// Receipts never generate receipts: an arriving one is recorded and skipped
/// before it reaches the inbox, so it is never among the messages this
/// reports on.
fn publish_delivery_receipts(
    context: &Context,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<()> {
    let database = Database::open(context.paths.database())?;
    let reported: std::collections::HashSet<String> = database
        .audit_entries(None)?
        .into_iter()
        .filter(|entry| {
            entry.channel_id.as_deref() == Some(&channel.channel_id)
                && entry.action == "delivery_receipt_queued"
        })
        .filter_map(|entry| entry.message_id)
        .collect();

    let by_sender =
        delivery_receipt_obligations(database.inbox_entries(&channel.channel_id)?, &reported);

    for ((sender, _sender_device), message_ids) in by_sender {
        for batch in message_ids.chunks(hrc_protocol::receipt::MAX_REFERENCED_MESSAGES) {
            let body = hrc_core::receipt::build_receipt(
                batch.iter().cloned(),
                hrc_protocol::ReceiptState::Delivered,
                now,
            )?;
            let body = canonical::to_canonical_json(&body)?;
            compose_body(
                context,
                hrc_protocol::MessageKind::Receipt,
                &sender,
                canonical::from_json_str(&body)?,
                None,
                None,
            )?;

            // A crash before these markers may repeat a report, which the
            // receiver deduplicates. Marking before queueing could lose it.
            for message_id in batch {
                database.append_audit(
                    Some(&channel.channel_id),
                    Some(message_id),
                    "delivery_receipt_queued",
                    None,
                    None,
                    now,
                )?;
            }
        }
    }

    Ok(())
}

fn delivery_receipt_obligations(
    entries: Vec<hrc_storage::InboxEntry>,
    reported: &std::collections::HashSet<String>,
) -> std::collections::BTreeMap<(String, String), Vec<String>> {
    let mut by_sender = std::collections::BTreeMap::new();
    for entry in entries {
        if entry.disposition != "expired" && !reported.contains(&entry.message_id) {
            by_sender
                .entry((entry.sender_principal, entry.sender_device))
                .or_insert_with(Vec::new)
                .push(entry.message_id);
        }
    }
    by_sender
}

/// Decrypts and records every message published since the last receive pass.
///
/// The walk replays control and message objects together, in publication
/// order, applying control entries to the roster as it goes. That ordering is
/// the point: a message must be opened against the roster as it stood when
/// the message was introduced, not as it stands now. Using the current epoch
/// would reject a message that was legitimate when it was sent and whose
/// sender has since been revoked — and accepting one under the wrong epoch
/// would do the opposite.
fn receive_messages(
    transport: &GitTransport,
    database: &mut Database,
    channel: &hrc_storage::ChannelRecord,
    identity: Option<&hrc_crypto::DeviceIdentity>,
    now: &str,
) -> Result<Vec<String>> {
    let mut roster: Option<Roster> = None;
    let mut received = Vec::new();
    // Rebuild the roster from genesis on every receive pass. The receive
    // cursor decides whether a pass is necessary, but resuming the transport
    // walk at an arbitrary data commit would leave `roster` empty until the
    // next control object and silently skip intervening ciphertext.
    let mut cursor = None;

    loop {
        let page = transport.fetch(cursor.as_deref(), 100)?;

        for publication in &page.publications {
            for object in &publication.objects {
                let bytes = transport.get_object(&object.name, &object.sha256)?;

                match object.class {
                    ObjectClass::Control => {
                        let text = std::str::from_utf8(&bytes).map_err(|_| {
                            hrc_transport::TransportError::InvalidPublication {
                                reason: format!("control object {} is not UTF-8", object.name),
                            }
                        })?;

                        match &mut roster {
                            None => {
                                let genesis: SignedObject<GenesisPayload> =
                                    canonical::from_json_str(text)?;
                                roster = Some(Roster::from_genesis(&genesis)?);
                            }
                            Some(roster) => {
                                let entry: SignedObject<ControlEntryPayload> =
                                    canonical::from_json_str(text)?;
                                roster.apply(&entry)?;
                            }
                        }
                    }

                    ObjectClass::Message => {
                        let Some(identity) = identity else {
                            continue;
                        };
                        let Some(roster) = roster.as_ref() else {
                            continue;
                        };

                        // A message this installation cannot decrypt is not
                        // an error: it was addressed to someone else, and a
                        // channel member seeing ciphertext they are not a
                        // recipient of is the ordinary case.
                        let Ok(opened) = hrc_core::message::open_for_ordering(
                            roster,
                            identity,
                            &bytes,
                            roster.epoch(),
                            now,
                        ) else {
                            continue;
                        };
                        let expired = opened.is_expired();
                        let opened = opened.message();
                        let local_recipient = roster
                            .devices()
                            .find(|device| {
                                device.encryption_recipient == identity.recipient().to_string()
                            })
                            .map(|device| device.device_id.as_str());
                        let recipient_previous_chain_id = local_recipient
                            .and_then(|device_id| opened.envelope.recipient_predecessor(device_id))
                            .flatten();
                        let recipient_device = opened
                            .envelope
                            .recipient_previous_chain_ids
                            .as_ref()
                            .and(local_recipient);

                        let is_receipt =
                            opened.envelope.kind == hrc_protocol::MessageKind::Receipt.as_str();
                        let received_context = if is_receipt {
                            Ok(None)
                        } else {
                            ContextPackage::from_message_body(&opened.envelope.body)
                        };
                        let malformed_context =
                            received_context.as_ref().err().map(ToString::to_string);
                        let received_context = received_context.ok().flatten();
                        let context_manifest = received_context
                            .as_ref()
                            .map(|(package, _)| canonical::to_canonical_bytes(package))
                            .transpose()?;
                        let context = received_context
                            .as_ref()
                            .zip(context_manifest.as_deref())
                            .map(
                                |((package, digest), manifest)| hrc_storage::InboundContext {
                                    package_id: &package.id,
                                    digest,
                                    manifest,
                                },
                            );
                        // For a context-bearing message the canonical body is
                        // the pending content. It owns the context lifecycle:
                        // trusted approval reveals/delivers this exact body,
                        // while agent-safe views never receive it.
                        let body = if received_context.is_some()
                            || malformed_context.is_some()
                            || !matches!(
                                opened.envelope.kind.as_str(),
                                "note" | "question" | "answer"
                            ) {
                            canonical::to_canonical_json(&opened.envelope.body)?
                        } else {
                            opened
                                .envelope
                                .body
                                .get("text")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned()
                        };
                        let plaintext_bytes =
                            canonical::to_canonical_bytes(&opened.envelope.body)?.len() as u64;

                        let kind = hrc_protocol::MessageKind::parse(&opened.envelope.kind)
                            .map(|kind| kind.as_str())
                            .unwrap_or("unsupported");

                        let chain_id = hrc_storage::chain_id_for(
                            &channel.channel_id,
                            &opened.sender_device,
                            opened.envelope.device_sequence,
                            &opened.envelope.message_id,
                        )?;

                        let outcome = database.record_inbound(
                            &hrc_storage::InboundMessage {
                                channel_id: &channel.channel_id,
                                message_id: &opened.envelope.message_id,
                                sender_principal: &opened.sender_principal,
                                sender_device: &opened.sender_device,
                                device_sequence: opened.envelope.device_sequence,
                                chain_id: &chain_id,
                                previous_chain_id: opened.envelope.previous_chain_id.as_deref(),
                                recipient_device,
                                recipient_previous_chain_id,
                                kind,
                                thread_id: Some(&opened.envelope.thread_id),
                                in_reply_to: opened.envelope.in_reply_to.as_deref(),
                                roster_epoch: opened.envelope.roster_epoch,
                                endpoint: opened.envelope.to.endpoint.as_deref(),
                                ciphertext_bytes: bytes.len() as u64,
                                plaintext_bytes,
                                ciphertext_sha256: &opened.ciphertext_sha256,
                                created_at: &opened.envelope.created_at,
                                expires_at: opened.envelope.expires_at.as_deref(),
                                expired,
                                attachment_count: u32::try_from(opened.envelope.attachments.len())
                                    .unwrap_or(u32::MAX),
                                attachment_bytes: opened.envelope.attachments.iter().fold(
                                    0u64,
                                    |total, attachment| {
                                        total.saturating_add(attachment.ciphertext_bytes)
                                    },
                                ),
                                prompt_request: opened.envelope.requested_capability.as_deref()
                                    == Some(hrc_core::gate::PROMPT_CAPABILITY),
                                body: body.as_bytes(),
                                ciphertext: &bytes,
                                context,
                            },
                            now,
                        )?;

                        match outcome {
                            hrc_storage::InboundOutcome::Accepted { released, .. } => {
                                received.push(opened.envelope.message_id.clone());
                                received.extend(released);
                            }
                            hrc_storage::InboundOutcome::ReceiptAccepted { released } => {
                                received.extend(released);
                            }
                            hrc_storage::InboundOutcome::ExpiredAccepted { released } => {
                                received.extend(released);
                            }
                            _ => {}
                        }
                        if is_receipt && !expired {
                            // A receipt's authenticated link must advance the
                            // sender chain even if its report is invalid.
                            // Storage keeps it out of the inbox and therefore
                            // out of prompt-gate decisions and receipt replies.
                            record_inbound_receipt(database, channel, opened, now)?;
                        }
                        if let Some(reason) = malformed_context {
                            // A valid signature authenticates that this
                            // sender authored malformed context. It does not
                            // turn a bad package into transport tampering.
                            database.reject_malformed_context(
                                &channel.channel_id,
                                &opened.envelope.message_id,
                                &reason,
                                now,
                            )?;
                        }
                    }

                    _ => {}
                }
            }
        }

        cursor = page.cursor;
        if !page.more {
            break;
        }
    }

    if let Some(roster) = roster {
        database.set_roster_progress(&channel.channel_id, roster.epoch(), roster.sequence())?;
    }
    if identity.is_some()
        && let Some(cursor) = cursor
    {
        database.set_receive_cursor(&channel.channel_id, &cursor)?;
    }

    Ok(received)
}

/// Verifies an arriving receipt and records what it reports.
///
/// The verification is the point. `accept_receipt` refuses a report naming a
/// message this installation did not send, or coming from a device that was
/// never among its recipients, and it refuses outright rather than recording
/// the claim and discounting it later — a stored claim tends to be read as a
/// fact.
///
/// A receipt that fails verification is dropped rather than raised. It is a
/// remote party's assertion about our own state, and the sender of a bad one
/// should not be able to interrupt synchronization for everyone by publishing
/// it.
fn record_inbound_receipt(
    database: &Database,
    channel: &hrc_storage::ChannelRecord,
    opened: &hrc_core::message::QuarantinedMessage,
    now: &str,
) -> Result<()> {
    let Ok(body) =
        serde_json::from_value::<hrc_protocol::receipt::ReceiptBody>(opened.envelope.body.clone())
    else {
        return Ok(());
    };

    let sent: Vec<hrc_core::receipt::SentMessage> = database
        .sent_messages(&channel.channel_id)?
        .into_iter()
        .map(|facts| hrc_core::receipt::SentMessage {
            message_id: facts.message_id,
            recipient_device_ids: facts.recipient_device_ids,
            thread_id: facts.thread_id,
            kind: facts.kind,
        })
        .collect();

    let Ok(verified) = hrc_core::receipt::accept_receipt(opened, &body, &sent) else {
        return Ok(());
    };

    for report in verified {
        database.record_receipt(
            &channel.channel_id,
            &hrc_storage::RecordedReceipt {
                message_id: report.message_id,
                reporter_principal: report.reporter_principal,
                reporter_device: report.reporter_device,
                state: report.state.as_str().to_owned(),
                rejection_code: report.rejection_code,
                reported_at: report.reported_at,
            },
            now,
        )?;
    }

    Ok(())
}

/// `hrc send`, `hrc ask`, and `hrc reply` share this path.
///
/// Sealing needs the roster, because the recipient set is derived from it
/// rather than supplied: a caller that could pass a list separately could
/// commit to one set and encrypt to another (HRC-SEC-016). Allocation comes
/// before sealing, so a crash between them loses a sequence number rather
/// than a message the user believes was sent.
fn compose(
    context: &Context,
    kind: hrc_protocol::MessageKind,
    recipient: &str,
    text: &str,
    in_reply_to: Option<&str>,
    expires: Option<&str>,
) -> Result<Value> {
    compose_body(
        context,
        kind,
        recipient,
        serde_json::json!({ "text": text }),
        in_reply_to,
        expires,
    )
}

/// Sends an arbitrary, structured message body through the ordinary durable
/// outbox and encrypted publication path.
fn compose_body(
    context: &Context,
    kind: hrc_protocol::MessageKind,
    recipient: &str,
    body: Value,
    in_reply_to: Option<&str>,
    expires: Option<&str>,
) -> Result<Value> {
    let store = context.key_store()?;
    let device: DeviceSecrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    let mut database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let channel = only_channel(&database)?;

    // An endpoint is advisory: it asks the receiving human to consider
    // routing the message somewhere, and decides nothing on their machine.
    let (principal_id, endpoint) = match recipient.split_once('/') {
        Some((principal_id, endpoint)) => (principal_id, Some(endpoint.to_owned())),
        None => (recipient, None),
    };

    // A lifetime is resolved to an absolute timestamp here, for the same
    // reason an invite's is: the protocol compares timestamps, and a signed
    // envelope carrying "24h" would lapse at a moment that depends on when
    // someone read it rather than when it was sent.
    let expires_at = match expires {
        Some(lifetime) => Some(expiry_from(&now, lifetime)?),
        None => None,
    };

    let mut transport = GitTransport::open(
        context.paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;
    transport.sync_from_remote()?;
    let (roster, _) = load_roster(&transport, &channel.channel_id)?;

    let device_id = local_device_id(&roster, &device)?;
    let message_id = hrc_protocol::ulid_at(unix_milliseconds()?)?;

    let (thread_id, in_reply_to) = match in_reply_to {
        // A reply continues the thread of what it answers, so the thread is
        // read from local state rather than chosen: a sender that picked its
        // own would be able to attach a reply to any conversation.
        Some(message_id) => {
            let entry = database
                .inbox_entries(&channel.channel_id)?
                .into_iter()
                .find(|entry| entry.message_id == message_id)
                .ok_or_else(|| CliError::NoSuchMessage {
                    message_id: message_id.to_owned(),
                })?;

            (
                entry.thread_id.unwrap_or_else(|| message_id.to_owned()),
                Some(message_id.to_owned()),
            )
        }
        None => (message_id.clone(), None),
    };

    let payload_hash = canonical::canonical_sha256_hex(&body)?;

    let envelope = MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: channel.channel_id.clone(),
        roster_epoch: roster.epoch(),
        message_id: message_id.clone(),
        device_sequence: 0,
        previous_chain_id: None,
        created_at: now.clone(),
        expires_at: expires_at.clone(),
        to: hrc_protocol::Addressing {
            principals: vec![principal_id.to_owned()],
            endpoint,
        },
        recipients: hrc_protocol::RecipientDevices::new([device_id.clone()])?,
        recipient_previous_chain_ids: None,
        thread_id: thread_id.clone(),
        in_reply_to,
        kind: kind.as_str().to_owned(),
        requested_capability: None,
        body,
        attachments: Vec::new(),
        padding: String::new(),
    };

    let addressed: Vec<String> = hrc_core::message::intended_recipients(&roster, &envelope)?
        .into_iter()
        .map(|device| device.device_id.clone())
        .collect();
    let local_recipient = device.device_identity()?.recipient();
    database.compose_outgoing(
        &channel.channel_id,
        &device_id,
        &message_id,
        roster.epoch(),
        &payload_hash,
        &thread_id,
        kind.as_str(),
        envelope.in_reply_to.as_deref(),
        &addressed,
        &now,
        |reservation| {
            let mut envelope = envelope.clone();
            envelope.device_sequence = reservation.device_sequence;
            envelope.previous_chain_id = reservation.previous_chain_id.clone();
            envelope.recipient_previous_chain_ids =
                Some(reservation.recipient_previous_chain_ids.clone());
            let reseal_plaintext = canonical::to_canonical_bytes(&envelope)?;
            let reseal_material =
                hrc_crypto::encrypt_to(std::slice::from_ref(&local_recipient), &reseal_plaintext)?;
            let ciphertext = hrc_core::message::seal_with_predecessors(
                &roster,
                &device.signing_key(),
                Signer {
                    principal_id: principal.signing_key().verifying_key().to_base64url(),
                    device_id: device_id.clone(),
                },
                envelope,
                reservation.recipient_previous_chain_ids.clone(),
            )?;
            Ok::<_, CliError>(hrc_storage::OutgoingBuild {
                ciphertext,
                reseal_material,
            })
        },
    )?;

    let outcomes = publish_pending_outgoing(
        &mut transport,
        &mut database,
        &channel.channel_id,
        Some(&device),
        5,
        &now,
    )?;
    let published = outcomes
        .iter()
        .any(|outcome| outcome.published.contains(&message_id));

    database.append_audit(
        Some(&channel.channel_id),
        Some(&message_id),
        "send",
        Some(&payload_hash),
        Some(kind.as_str()),
        &now,
    )?;

    Ok(json!({
        "status": "ok",
        "messageId": message_id,
        "threadId": thread_id,
        "kind": kind.as_str(),
        "published": published,
        "deferred": !published,
    }))
}

/// `hrc send`: an informational note.
pub fn send(
    context: &Context,
    recipient: &str,
    text: &str,
    expires: Option<&str>,
) -> Result<Value> {
    compose(
        context,
        hrc_protocol::MessageKind::Note,
        recipient,
        text,
        None,
        expires,
    )
}

/// `hrc ask`: a question, optionally addressed to a logical endpoint.
pub fn ask(context: &Context, recipient: &str, text: &str, expires: Option<&str>) -> Result<Value> {
    compose(
        context,
        hrc_protocol::MessageKind::Question,
        recipient,
        text,
        None,
        expires,
    )
}

/// `hrc reply`: an answer in the thread of an existing message.
pub fn reply(context: &Context, message_id: &str, text: &str) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let entry = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id)
        .ok_or_else(|| CliError::NoSuchMessage {
            message_id: message_id.to_owned(),
        })?;

    compose(
        context,
        hrc_protocol::MessageKind::Answer,
        &entry.sender_principal,
        text,
        Some(message_id),
        // A reply inherits no expiry from the question it answers: expiry is
        // the sender's statement about their own message.
        None,
    )
}

/// `hrc delegate`: ask someone to do something, without being able to make
/// them.
///
/// The `task` kind is communication, never execution. `TaskBody` is proven to
/// expose no command, script, argument or environment field, and nothing on
/// the receiving side runs anything: a delegation arrives as a quarantined
/// message like any other and waits for a human.
///
/// `--context` names a package rather than carrying one. Disclosure is `hrc
/// context send`, which is a trusted operation with its own authorization, so
/// pointing at a package a recipient may already hold stays on the agent-safe
/// path where drafting a request belongs.
pub fn delegate(
    context: &Context,
    recipient: &str,
    title: &str,
    description: &str,
    criteria: &[String],
    context_id: Option<&str>,
    due: Option<&str>,
) -> Result<Value> {
    let due_by = match due {
        Some(lifetime) => {
            let database = Database::open(context.paths.database())?;
            let now = database.utc_now()?;
            Some(expiry_from(&now, lifetime)?)
        }
        None => None,
    };

    let body = hrc_protocol::delegation::TaskBody {
        title: title.to_owned(),
        description: description.to_owned(),
        acceptance_criteria: criteria.to_vec(),
        context_id: context_id.map(str::to_owned),
        due_by,
    };

    // Validated here rather than left to the receiver. A task with an empty
    // title or description is one nobody can act on, and finding that out
    // after it is published to an append-only history helps no one.
    body.validate()?;

    let mut sent = compose_body(
        context,
        hrc_protocol::MessageKind::Task,
        recipient,
        serde_json::to_value(&body).map_err(|_| {
            CliError::Core(hrc_core::CoreError::MalformedMessage {
                reason: "the delegation body could not be serialized".into(),
            })
        })?,
        None,
        // The task's own `dueBy` is when the requester stops waiting, which
        // is not the same as when the message should stop existing. Expiring
        // the message would delete the request from the recipient's inbox.
        None,
    )?;

    sent["title"] = Value::String(title.to_owned());
    Ok(sent)
}

/// `hrc context draft`: keep an explicitly selected package locally.
pub fn context_draft(
    context: &Context,
    manifest_path: &str,
    repository: Option<&str>,
) -> Result<Value> {
    let manifest = std::fs::read_to_string(manifest_path).map_err(|source| CliError::Io {
        action: "read the context manifest",
        source,
    })?;
    let package: ContextPackage = canonical::from_json_str(&manifest)?;
    if package.version != hrc_protocol::PROTOCOL_VERSION {
        return Err(hrc_core::CoreError::MalformedMessage {
            reason: "context package has an unsupported version".into(),
        }
        .into());
    }
    let mut preview = package.preview()?;
    let repository_root = canonical_repository_root(repository)?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    if let Some(excluded) = preview.excluded.first() {
        return Err(hrc_core::CoreError::ContextContainsExcludedPath {
            path: excluded.path.clone(),
        }
        .into());
    }

    // The manifest's excerpt text is untrusted caller input. Replace it with
    // the exact selected source bytes before computing the stored digest.
    let package = materialize_excerpt_sources(package, repository_root.as_deref())?;
    let package = materialize_captures(package, repository_root.as_deref())?;
    let canonical_manifest = canonical::to_canonical_bytes(&package)?;
    let digest = package.digest()?;
    let mut preview = package.preview()?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    // Drafting has no side effect outside this installation, so findings are
    // retained for the required human preview instead of silently removing
    // content or making the author reconstruct what was rejected.
    let database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    database.save_context_draft(
        &package.id,
        repository_root.as_deref(),
        &digest,
        &canonical_manifest,
        &now,
    )?;

    Ok(context_preview_value(
        &package.id,
        &digest,
        &preview,
        "draft",
    ))
}

/// `hrc context preview`: report the selected bytes and blockers without
/// echoing package text or a suspected secret.
pub fn context_preview(context: &Context, package_id: &str) -> Result<Value> {
    let (package, digest, repository_root) = load_context_draft(context, package_id)?;
    let mut preview = package.preview()?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    verify_excerpt_sources(&package, repository_root.as_deref())?;
    Ok(context_preview_value(
        package_id, &digest, &preview, "preview",
    ))
}

/// `hrc context send`: attach the exact reviewed package to an encrypted note.
pub fn context_send(context: &Context, recipient: &str, package_id: &str) -> Result<Value> {
    let (package, digest, repository_root) = load_context_draft(context, package_id)?;
    let mut preview = package.preview()?;
    append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)?;
    verify_excerpt_sources(&package, repository_root.as_deref())?;
    if !preview.secrets.is_empty() {
        return Err(hrc_core::CoreError::ContextContainsSecrets {
            count: preview.secrets.len(),
        }
        .into());
    }
    if let Some(excluded) = preview.excluded.first() {
        return Err(hrc_core::CoreError::ContextContainsExcludedPath {
            path: excluded.path.clone(),
        }
        .into());
    }

    let mut sent = compose_body(
        context,
        hrc_protocol::MessageKind::Note,
        recipient,
        serde_json::json!({
            "context": package,
            "contextDigest": digest,
        }),
        None,
        None,
    )?;
    sent["contextId"] = Value::String(package_id.to_owned());
    sent["contextDigest"] = Value::String(digest);
    Ok(sent)
}

fn load_context_draft(
    context: &Context,
    package_id: &str,
) -> Result<(ContextPackage, String, Option<String>)> {
    let database = Database::open(context.paths.database())?;
    let draft = database
        .context_draft(package_id)?
        .ok_or_else(|| CliError::NoSuchMessage {
            message_id: package_id.to_owned(),
        })?;
    let text = std::str::from_utf8(&draft.manifest).map_err(|_| {
        CliError::Core(hrc_core::CoreError::MalformedMessage {
            reason: "stored context manifest is not UTF-8".into(),
        })
    })?;
    let package = canonical::from_json_str::<ContextPackage>(text)?;
    package.verify_digest(&draft.digest)?;
    if package.id != draft.package_id {
        return Err(hrc_core::CoreError::MalformedMessage {
            reason: "stored context package ID does not match its record".into(),
        }
        .into());
    }
    Ok((package, draft.digest, draft.repository_root))
}

fn context_preview_value(
    package_id: &str,
    digest: &str,
    preview: &hrc_core::ContextPreview,
    state: &str,
) -> Value {
    json!({
        "status": "ok",
        "state": state,
        "contextId": package_id,
        "digest": digest,
        "totalBytes": preview.total_bytes,
        "items": preview.items.iter().map(|(kind, bytes)| json!({
            "kind": kind,
            "bytes": bytes,
        })).collect::<Vec<_>>(),
        "sendable": preview.is_sendable(),
        "secretFindings": preview.secrets.iter().map(|finding| json!({
            "item": finding.item_index,
            "rule": finding.rule,
            "detail": finding.detail,
        })).collect::<Vec<_>>(),
        "excludedPaths": preview.excluded.iter().map(|excluded| json!({
            "item": excluded.item_index,
            "path": excluded.path,
            "reason": excluded.reason,
        })).collect::<Vec<_>>(),
    })
}

/// Adds exclusions that only the source repository can decide.
fn append_repository_exclusions(
    package: &ContextPackage,
    repository: Option<&str>,
    preview: &mut hrc_core::ContextPreview,
) -> Result<()> {
    let source_paths = package.source_paths().collect::<Vec<_>>();
    if source_paths.is_empty() {
        return Ok(());
    }

    let repository = repository.ok_or_else(|| CliError::ContextRepositoryRequired {
        package_id: package.id.clone(),
    })?;

    for (item_index, path) in source_paths {
        if !is_repository_relative_path(path) {
            preview.excluded.push(ExcludedPath {
                item_index,
                path: path.to_owned(),
                reason: "excerpt paths must be repository-relative",
            });
            continue;
        }
        let normalized_path = path.replace('\\', "/");

        let status = ProcessCommand::new("git")
            .args([
                "-C",
                repository,
                "check-ignore",
                "--quiet",
                "--no-index",
                "--",
            ])
            .arg(&normalized_path)
            .status()
            .map_err(|source| CliError::Io {
                action: "check whether a context path is Git-ignored",
                source,
            })?;
        if status.success() {
            preview.excluded.push(ExcludedPath {
                item_index,
                path: path.to_owned(),
                reason: "path is ignored by its Git repository",
            });
        } else if status.code() != Some(1) {
            return Err(CliError::Io {
                action: "check whether a context path is Git-ignored",
                source: std::io::Error::other("Git could not evaluate the source repository"),
            });
        }
    }

    Ok(())
}

/// Resolves a supplied repository to its canonical absolute Git worktree
/// root. A relative display label is not a durable source identity.
fn canonical_repository_root(repository: Option<&str>) -> Result<Option<String>> {
    let Some(repository) = repository else {
        return Ok(None);
    };
    let output = ProcessCommand::new("git")
        .args(["-C", repository, "rev-parse", "--show-toplevel"])
        .output()
        .map_err(|source| CliError::Io {
            action: "resolve the context repository root",
            source,
        })?;
    if !output.status.success() {
        return Err(CliError::InvalidContextSource {
            reason: "repository is not a Git worktree",
        });
    }
    let root = String::from_utf8(output.stdout).map_err(|_| CliError::InvalidContextSource {
        reason: "Git returned a non-UTF-8 repository path",
    })?;
    let root = std::fs::canonicalize(root.trim()).map_err(|source| CliError::Io {
        action: "canonicalize the context repository root",
        source,
    })?;
    Ok(Some(root.display().to_string()))
}

/// Replaces every caller-supplied excerpt with bytes from its declared,
/// canonical repository source before the package digest is calculated.
fn materialize_excerpt_sources(
    mut package: ContextPackage,
    repository_root: Option<&str>,
) -> Result<ContextPackage> {
    if package.source_paths().next().is_some() && repository_root.is_none() {
        return Err(CliError::ContextRepositoryRequired {
            package_id: package.id.clone(),
        });
    }
    for item in &mut package.items {
        if let hrc_core::ContextItem::Excerpt {
            path,
            first_line,
            last_line,
            commit_sha,
            text,
        } = item
        {
            *text = read_excerpt(
                repository_root.expect("checked above"),
                path,
                *first_line,
                *last_line,
                commit_sha.as_deref(),
            )?;
        }
    }
    Ok(package)
}

/// The largest capture HRC will carry into a context package.
///
/// A bound rather than a limit chosen for elegance: a repository with a
/// thousand modified files produces a `git status` nobody will read and a
/// message that costs everyone bandwidth. Truncation is reported in the
/// text, so a reader never mistakes a cut-off capture for a complete one.
const MAX_CAPTURE_BYTES: usize = 64 * 1024;

/// Replaces requested patch and output items with what HRC captured itself
/// (PRD requirements HRC-CTX-004 and HRC-CTX-007).
///
/// This is the whole point of those two rows. A caller names *what* to
/// capture — a revision range, or one command from
/// [`hrc_core::context::AllowedCommand`] — and HRC produces the bytes. Text
/// the caller supplied is discarded rather than trusted, exactly as
/// [`materialize_excerpt_sources`] does for excerpts, so provenance is
/// derived and not asserted.
fn materialize_captures(
    mut package: ContextPackage,
    repository_root: Option<&str>,
) -> Result<ContextPackage> {
    let needs_capture = package.items.iter().any(|item| {
        matches!(
            item,
            hrc_core::ContextItem::Patch { .. } | hrc_core::ContextItem::Output { .. }
        )
    });

    if !needs_capture {
        return Ok(package);
    }

    let Some(root) = repository_root else {
        return Err(CliError::ContextRepositoryRequired {
            package_id: package.id.clone(),
        });
    };

    for item in &mut package.items {
        match item {
            hrc_core::ContextItem::Patch { range, diff } => {
                // A range is caller input that reaches a command line, so it
                // is checked against a conservative shape rather than passed
                // through. `--output=/etc/passwd` is a revision range as far
                // as a naive check is concerned.
                if !is_plain_revision_range(range) {
                    // The rejected range is deliberately not echoed: this
                    // field is `&'static str` so caller input cannot reach a
                    // diagnostic, and a range is caller input.
                    return Err(CliError::InvalidContextSource {
                        reason: "a patch range must be a plain revision range such as \
                                 HEAD~3..HEAD",
                    });
                }

                *diff = capture_git(root, &["diff", "--no-color", range])?;
            }
            hrc_core::ContextItem::Output { command, text } => {
                let allowed = hrc_core::context::AllowedCommand::parse(command).ok_or(
                    CliError::InvalidContextSource {
                        reason: "output items may carry only the output of a command HRC runs \
                                 itself; see AllowedCommand for the list",
                    },
                )?;

                *text = capture_git(root, &allowed.argv())?;
            }
            _ => {}
        }
    }

    Ok(package)
}

/// Whether a string is a revision range and nothing else.
///
/// Deliberately narrow. Git accepts a great deal of syntax, and the ones
/// that matter here are the ones that stop being a range: anything starting
/// with `-` is an option, and whitespace makes it more than one argument.
fn is_plain_revision_range(range: &str) -> bool {
    let range = range.trim();

    !range.is_empty()
        && !range.starts_with('-')
        && range.len() <= 200
        && range.chars().all(|character| {
            character.is_ascii_alphanumeric()
                || matches!(character, '.' | '_' | '-' | '/' | '~' | '^' | '@')
        })
}

/// Runs one read-only Git command in the repository and returns its output.
fn capture_git(root: &str, arguments: &[&str]) -> Result<String> {
    let mut command = ProcessCommand::new("git");
    command.args(["-C", root]).args(arguments);

    let output = command.output().map_err(|source| CliError::Io {
        action: "capture context from the repository",
        source,
    })?;

    if !output.status.success() {
        // Git's stderr can quote refs and paths from the repository, so it
        // is not repeated here for the same reason the range is not.
        return Err(CliError::InvalidContextSource {
            reason: "the repository could not produce that capture",
        });
    }

    let captured = String::from_utf8_lossy(&output.stdout).into_owned();
    Ok(truncate_capture(captured))
}

/// Bounds a capture, saying so in the text when it was cut.
fn truncate_capture(captured: String) -> String {
    if captured.len() <= MAX_CAPTURE_BYTES {
        return captured;
    }

    // Cut on a character boundary, then on a line, so the result is neither
    // invalid UTF-8 nor a half-written path.
    let mut cut = MAX_CAPTURE_BYTES;
    while cut > 0 && !captured.is_char_boundary(cut) {
        cut -= 1;
    }
    let head = &captured[..cut];
    let head = head.rfind('\n').map_or(head, |line| &head[..line]);

    format!("{head}\n[truncated by HRC at {MAX_CAPTURE_BYTES} bytes]\n")
}

/// Confirms that the checked source still equals the snapshot a human will
/// review and authorize.
fn verify_excerpt_sources(package: &ContextPackage, repository_root: Option<&str>) -> Result<()> {
    for item in &package.items {
        if let hrc_core::ContextItem::Excerpt {
            path,
            first_line,
            last_line,
            commit_sha,
            text,
        } = item
        {
            let actual = read_excerpt(
                repository_root.ok_or_else(|| CliError::ContextRepositoryRequired {
                    package_id: package.id.clone(),
                })?,
                path,
                *first_line,
                *last_line,
                commit_sha.as_deref(),
            )?;
            if actual != *text {
                return Err(CliError::InvalidContextSource {
                    reason: "excerpt source changed since the package was drafted",
                });
            }
        }
    }
    Ok(())
}

/// Reads one bounded source selection without accepting manifest text.
fn read_excerpt(
    repository_root: &str,
    path: &str,
    first_line: u32,
    last_line: u32,
    commit_sha: Option<&str>,
) -> Result<String> {
    if !is_repository_relative_path(path) {
        return Err(CliError::InvalidContextSource {
            reason: "excerpt paths must be repository-relative",
        });
    }
    if first_line == 0 || last_line < first_line {
        return Err(CliError::InvalidContextSource {
            reason: "excerpt line range is invalid",
        });
    }

    let source = if let Some(commit_sha) = commit_sha {
        if !matches!(commit_sha.len(), 40 | 64)
            || !commit_sha.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt commit must be a full SHA-1 or SHA-256 object ID",
            });
        }
        let commit_expression = format!("{commit_sha}^{{commit}}");
        let verified = ProcessCommand::new("git")
            .args(["-C", repository_root, "rev-parse", "--verify"])
            .arg(&commit_expression)
            .output()
            .map_err(|source| CliError::Io {
                action: "verify the committed context excerpt",
                source,
            })?;
        if !verified.status.success() {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt commit does not resolve to a commit",
            });
        }
        let output = ProcessCommand::new("git")
            .args(["-C", repository_root, "show", "--no-textconv"])
            .arg(format!("{commit_sha}:{}", path.replace('\\', "/")))
            .output()
            .map_err(|source| CliError::Io {
                action: "read the committed context excerpt",
                source,
            })?;
        if !output.status.success() {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt commit or path is unavailable",
            });
        }
        String::from_utf8(output.stdout).map_err(|_| CliError::InvalidContextSource {
            reason: "excerpt source is not UTF-8 text",
        })?
    } else {
        let root = std::fs::canonicalize(repository_root).map_err(|source| CliError::Io {
            action: "canonicalize the context repository root",
            source,
        })?;
        let source_path =
            std::fs::canonicalize(root.join(path.replace('\\', "/"))).map_err(|source| {
                CliError::Io {
                    action: "read the context excerpt",
                    source,
                }
            })?;
        if !source_path.starts_with(&root) {
            return Err(CliError::InvalidContextSource {
                reason: "excerpt path escapes its repository",
            });
        }
        std::fs::read_to_string(&source_path).map_err(|source| CliError::Io {
            action: "read the context excerpt",
            source,
        })?
    };

    let selected = source
        .lines()
        .skip((first_line - 1) as usize)
        .take((last_line - first_line + 1) as usize)
        .collect::<Vec<_>>()
        .join("\n");
    if selected.lines().count() != (last_line - first_line + 1) as usize {
        return Err(CliError::InvalidContextSource {
            reason: "excerpt line range is outside the source file",
        });
    }
    Ok(selected)
}

fn is_repository_relative_path(path: &str) -> bool {
    // Context manifests move between Windows and Unix. Treat both separators
    // as separators before asking the host's path parser, or a Windows
    // traversal could become an ordinary filename on Unix.
    let normalized = path.replace('\\', "/");
    let path = Path::new(&normalized);
    !path.as_os_str().is_empty()
        && !path.is_absolute()
        && path.components().all(|component| {
            !matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
}

/// `hrc inbox`: the closed metadata set, never a pending body.
pub fn inbox(context: &Context, pending_only: bool) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let entries: Vec<Value> = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .filter(|entry| !pending_only || entry.disposition == "quarantined")
        .map(|entry| {
            json!({
                "messageId": entry.message_id,
                "sender": entry.sender_principal,
                "kind": entry.kind,
                "threadId": entry.thread_id,
                "inReplyTo": entry.in_reply_to,
                "arrival": entry.arrival_sequence,
                "expiresAt": entry.expires_at,
                "disposition": entry.disposition,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "channelId": channel.channel_id,
        "entries": entries,
    }))
}

/// `hrc show`: everything known about one message, without disclosing a
/// body nobody approved.
///
/// The rule is the same one the inbox and the plugin pane follow, and it is
/// why this is not simply "print the message". An inbound body stays sealed
/// until a human releases it through the trusted interface, so what this
/// shows for a quarantined message is the agent-safe metadata and the reason
/// the body is absent — never the body itself.
///
/// A message this installation sent is different in kind: the content was
/// never quarantined, because it did not arrive from anyone. What matters
/// there is where it got to, which is the outbox state and the receipts
/// other devices published about it.
pub fn show(context: &Context, message_id: &str) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let inbound = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id);

    if let Some(entry) = inbound {
        // `released_content` returns something only for a decision that
        // actually delivers. Keeping a message in the inbox and declining it
        // are decisions too, and neither authorizes disclosure.
        let released = released_content(&database, message_id)?;

        return Ok(json!({
            "status": "ok",
            "messageId": entry.message_id,
            "direction": "inbound",
            "sender": entry.sender_principal,
            "kind": entry.kind,
            "threadId": entry.thread_id,
            "inReplyTo": entry.in_reply_to,
            "arrival": entry.arrival_sequence,
            "expiresAt": entry.expires_at,
            "disposition": entry.disposition,
            "body": match released {
                Some(text) => Value::String(text),
                None => Value::Null,
            },
            "bodyWithheld": match entry.disposition.as_str() {
                "quarantined" => Some("awaiting a decision in the trusted interface"),
                "declined" => Some("declined"),
                "expired" => Some("expired before a decision"),
                _ => None,
            },
        }));
    }

    let Some(state) = database.outbox_state(message_id)? else {
        return Err(CliError::NoSuchMessage {
            message_id: message_id.to_owned(),
        });
    };

    let receipts: Vec<Value> = database
        .receipts_for(message_id)?
        .into_iter()
        .map(|receipt| {
            json!({
                "reporter": receipt.reporter_principal,
                "device": receipt.reporter_device,
                "state": receipt.state,
                "rejectionCode": receipt.rejection_code,
                "reportedAt": receipt.reported_at,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "messageId": message_id,
        "direction": "outbound",
        "state": state.as_str(),
        "receipts": receipts,
    }))
}

/// `hrc wait`: block until a message reaches a state, or give up.
///
/// The states are the ones the rest of the system already records, not a new
/// vocabulary: an outbound message reaches `published` in the outbox and then
/// `delivered`, `read`, `accepted` or `rejected` when a recipient device says
/// so, and an inbound message reaches a disposition when a human decides.
///
/// Each pass runs a real synchronization rather than only reading local
/// state. Without one, waiting on a machine whose daemon is not running would
/// block until the timeout no matter what the channel did — and section 17.1
/// names explicit `hrc wait` as the reason a five-second poll exists.
pub fn wait(
    context: &Context,
    message_id: &str,
    until: Option<&str>,
    timeout: Option<&str>,
) -> Result<Value> {
    const DEFAULT_STATE: &str = "delivered";
    const DEFAULT_TIMEOUT_SECONDS: u64 = 300;

    let wanted = until.unwrap_or(DEFAULT_STATE);
    let limit = match timeout {
        Some(value) => Duration::from_secs(parse_wait_timeout(value)?),
        None => Duration::from_secs(DEFAULT_TIMEOUT_SECONDS),
    };

    // Section 17.1's explicit-wait interval, and the adapter's own minimum
    // where that is longer: a provider's rate limit is a hard constraint and
    // exceeding it throttles every channel the installation hosts.
    let interval = {
        let database = Database::open(context.paths.database())?;
        let channel = only_channel(&database)?;
        let transport = GitTransport::open(
            context.paths.channel_transport(&channel.channel_id),
            &channel.transport_locator,
        )?;
        let minimum =
            Duration::from_secs(transport.capabilities().min_poll_interval_seconds as u64);
        poll_interval(PollActivity::Waiting, minimum)
    };

    let started = std::time::Instant::now();

    loop {
        if let Some(state) = reached_state(context, message_id, wanted)? {
            return Ok(json!({
                "status": "ok",
                "messageId": message_id,
                "state": state,
                "waitedSeconds": started.elapsed().as_secs(),
            }));
        }

        if started.elapsed() >= limit {
            // A timeout is not an error. The caller asked how things stand
            // after a bounded wait, and "not yet" is an answer to that.
            return Ok(json!({
                "status": "ok",
                "messageId": message_id,
                "state": "timeout",
                "waitedSeconds": started.elapsed().as_secs(),
            }));
        }

        std::thread::sleep(interval.min(limit.saturating_sub(started.elapsed())));
        if started.elapsed() >= limit {
            continue;
        }

        // Failing to reach the transport is not a reason to stop waiting: the
        // state may still arrive through a daemon running alongside this, and
        // a caller that asked for a timeout asked to be told at the end of it.
        if let Err(error) = sync_once(context) {
            let database = Database::open(context.paths.database())?;
            database.append_audit(
                None,
                Some(message_id),
                "wait_sync_failed",
                None,
                Some(&error.to_string()),
                &database.utc_now()?,
            )?;
        }
    }
}

/// The state a message is in now, in the vocabulary `hrc wait` accepts.
///
/// Receipts win over the outbox for an outbound message, because `published`
/// is what this installation did and a receipt is what happened to it. The
/// most advanced report is the one returned: a device that has read a message
/// also received it, and reporting `delivered` after `read` would go
/// backwards.
fn reached_state(context: &Context, message_id: &str, wanted: &str) -> Result<Option<String>> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;
    let mut observed = Vec::new();

    if let Some(entry) = database
        .inbox_entries(&channel.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id)
    {
        observed.push(entry.disposition);
    }

    let reported = database.receipts_for(message_id)?;
    for state in ["rejected", "accepted", "read", "delivered"] {
        if reported.iter().any(|receipt| receipt.state == state) {
            observed.push(state.to_owned());
        }
    }

    if let Some(state) = database.outbox_state(message_id)? {
        observed.push(state.as_str().to_owned());
    }
    Ok(observed
        .into_iter()
        .filter(|state| state_reaches(state, wanted))
        .max_by_key(|state| state_rank(state)))
}

fn state_rank(state: &str) -> u8 {
    match state {
        "accepted" | "approved" | "edited" => 9,
        "read" => 8,
        "delivered" => 7,
        "rejected" | "declined" | "expired" => 6,
        "published" => 5,
        "publishing" => 4,
        "queued" => 3,
        "reserved" => 2,
        "quarantined" => 1,
        _ => 0,
    }
}

fn state_reaches(observed: &str, wanted: &str) -> bool {
    if observed == wanted {
        return true;
    }
    match wanted {
        "reserved" => matches!(
            observed,
            "queued" | "publishing" | "published" | "delivered" | "read" | "accepted" | "rejected"
        ),
        "queued" => matches!(
            observed,
            "publishing" | "published" | "delivered" | "read" | "accepted" | "rejected"
        ),
        "publishing" | "published" => {
            matches!(
                observed,
                "published" | "delivered" | "read" | "accepted" | "rejected"
            )
        }
        "delivered" => matches!(observed, "read" | "accepted"),
        "read" => observed == "accepted",
        "quarantined" => matches!(observed, "approved" | "edited" | "declined" | "expired"),
        _ => false,
    }
}

/// Parses `30s`, `5m`, or `2h` into seconds.
///
/// Separate from `parse_lifetime`, which governs invite and message expiry on
/// the wire. That one deliberately has no seconds unit, because a lifetime
/// measured in seconds is not a useful thing to publish; a wait measured in
/// seconds is entirely reasonable.
fn parse_wait_timeout(value: &str) -> Result<u64> {
    let invalid = || CliError::InvalidLifetime {
        value: value.to_owned(),
    };

    let value = value.trim();
    let (digits, unit) = value
        .split_at_checked(value.len().checked_sub(1).ok_or_else(invalid)?)
        .ok_or_else(invalid)?;
    let amount: u64 = digits.parse().map_err(|_| invalid())?;

    if amount == 0 {
        return Err(invalid());
    }

    match unit {
        "s" => Ok(amount),
        "m" => amount.checked_mul(60).ok_or_else(invalid),
        "h" => amount.checked_mul(3_600).ok_or_else(invalid),
        _ => Err(invalid()),
    }
}

/// `hrc thread`: one conversation in local arrival order.
///
/// Pending bodies stay redacted. Ordering is by arrival rather than by the
/// sender's timestamp, which a sender chooses.
pub fn thread(context: &Context, thread_id: &str) -> Result<Value> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;

    let entries: Vec<Value> = database
        .thread_entries(&channel.channel_id, thread_id)?
        .into_iter()
        .map(|entry| {
            json!({
                "messageId": entry.message_id,
                "sender": entry.sender_principal,
                "kind": entry.kind,
                "arrival": entry.arrival_sequence,
                "disposition": entry.disposition,
                "body": if entry.disposition == "quarantined" {
                    Value::String("[redacted until approved]".into())
                } else {
                    Value::Null
                },
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "threadId": thread_id,
        "entries": entries,
    }))
}

/// Milliseconds since the Unix epoch, for a message identifier.
///
/// Read from the system clock rather than derived from the RFC 3339
/// timestamp the rest of the record uses, because that one has second
/// precision. A ULID built from it would give every message sent within the
/// same second an identical time prefix, and their order would then be
/// decided by the random half — which is to say, not ordered at all.
///
/// Sub-second ties remain possible and remain unordered; that is what a ULID
/// promises and no more. Per-device order does not depend on this: the chain
/// in section 18.1 establishes it, and the inbox reads by arrival.
fn unix_milliseconds() -> Result<u64> {
    let since_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|_| CliError::Entropy)?;

    Ok(since_epoch.as_millis() as u64)
}

/// Turns a lifetime such as `24h` into an absolute RFC 3339 expiry.
///
/// The protocol compares timestamps, not durations: an invite carrying "24h"
/// would be compared lexicographically against a date and lapse at some
/// arbitrary moment that depends on the year. Resolving it here means the
/// value the channel sees is one every participant can evaluate.
fn expiry_from(now: &str, lifetime: &str) -> Result<String> {
    let seconds = parse_lifetime(lifetime).ok_or_else(|| CliError::InvalidLifetime {
        value: lifetime.to_owned(),
    })?;

    let start = time_from_rfc3339(now).ok_or_else(|| CliError::InvalidLifetime {
        value: now.to_owned(),
    })?;

    Ok(rfc3339_from(start + seconds))
}

/// Parses `30m`, `24h`, or `7d` into seconds.
fn parse_lifetime(value: &str) -> Option<i64> {
    let value = value.trim();
    let (digits, unit) = value.split_at(value.len().checked_sub(1)?);
    let amount: i64 = digits.parse().ok()?;

    if amount <= 0 {
        return None;
    }

    match unit {
        "m" => Some(amount * 60),
        "h" => Some(amount * 3600),
        "d" => Some(amount * 86_400),
        _ => None,
    }
}

/// Seconds since the Unix epoch for an RFC 3339 UTC timestamp.
///
/// Only the shape this project emits is accepted: `YYYY-MM-DDTHH:MM:SSZ`.
/// Accepting more would mean accepting offsets and fractions nothing here
/// produces, and quietly mis-parsing one of those is worse than refusing it.
fn time_from_rfc3339(value: &str) -> Option<i64> {
    let bytes = value.as_bytes();
    if bytes.len() != 20 || bytes[4] != b'-' || bytes[10] != b'T' || bytes[19] != b'Z' {
        return None;
    }

    let field = |range: std::ops::Range<usize>| value.get(range)?.parse::<i64>().ok();
    let (year, month, day) = (field(0..4)?, field(5..7)?, field(8..10)?);
    let (hour, minute, second) = (field(11..13)?, field(14..16)?, field(17..19)?);

    Some(days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second)
}

/// Renders seconds since the Unix epoch as an RFC 3339 UTC timestamp.
pub(crate) fn rfc3339_from(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let remainder = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);

    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        remainder / 3600,
        (remainder % 3600) / 60,
        remainder % 60
    )
}

/// Days since the Unix epoch for a civil date (Howard Hinnant's algorithm).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = year - i64::from(month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (month + if month > 2 { -3 } else { 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;

    era * 146_097 + day_of_era - 719_468
}

/// The inverse of [`days_from_civil`].
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let days = days + 719_468;
    let era = days.div_euclid(146_097);
    let day_of_era = days - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_index + 2) / 5 + 1;
    let month = month_index + if month_index < 10 { 3 } else { -9 };

    (year + i64::from(month <= 2), month, day)
}

/// Where an invite secret is kept in the key store.
///
/// Invite identifiers are base64url, which contains `-` and `_` but no path
/// separator, so this cannot address anything outside the store directory.
fn invite_store_name(invite_id: &str) -> String {
    format!("invite-{invite_id}")
}

/// This installation's device, as the roster knows it.
fn local_device_id(roster: &Roster, device: &DeviceSecrets) -> Result<String> {
    let signing_key = device.signing_key().verifying_key().to_base64url();

    roster
        .devices()
        .find(|candidate| candidate.signing_key == signing_key)
        .map(|candidate| candidate.device_id.clone())
        .ok_or(CliError::LocalDeviceNotInChannel)
}

/// A fresh descriptor nonce.
///
/// Two devices created in the same second with the same keys would otherwise
/// share a descriptor, and therefore a device ID (PRD section 18.0.1).
fn random_nonce() -> Result<[u8; 16]> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| CliError::Entropy)?;
    Ok(nonce)
}

/// `hrc whoami`: report this device's public identity.
pub fn whoami(context: &Context) -> Result<Value> {
    let store = context.key_store()?;
    let secrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;

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
    let mut database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;

    // Reap before fetching. A message that lapsed while this installation
    // was offline should not appear as actionable for the length of one
    // synchronization pass (PRD requirement HRC-MSG-005).
    let expired = sweep_expired(&database, &now)?;

    let mut channels = Vec::new();
    for channel in database.channels()? {
        channels.push(sync_git_channel(context, &mut database, &channel, &now)?);
    }

    Ok(json!({
        "status": "ok",
        "syncedAt": now,
        "expired": expired,
        "channels": channels,
    }))
}

/// Marks lapsed messages and records one audit entry for each.
///
/// Expiry is the one inbox transition nobody decides: it is what happens
/// when no one says anything (PRD section 18.4). It is still recorded,
/// because "why did this never reach me" is a question the audit log should
/// be able to answer.
fn sweep_expired(database: &Database, now: &str) -> Result<Vec<String>> {
    let expired = database.sweep_expired(now)?;

    for message_id in &expired {
        database.append_audit(
            None,
            Some(message_id),
            "message_expired",
            None,
            Some("the message lapsed before a local decision was made"),
            now,
        )?;
    }

    Ok(expired)
}

/// `hrc daemon`: stay resident and keep synchronizing in the background.
pub fn daemon_tick(context: &Context) -> Result<Value> {
    let mut database = Database::open(context.paths.database())?;
    let now = database.utc_now()?;
    let expired = sweep_expired(&database, &now)?;
    let mut channels = Vec::new();
    let mut healthy = true;

    for channel in database.channels()? {
        match sync_git_channel(context, &mut database, &channel, &now) {
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
        "expired": expired,
        "channels": channels,
    }))
}

/// Runs the resident daemon loop until the process is stopped.
pub async fn daemon(context: &Context, announce: impl FnOnce(Value) -> Result<()>) -> Result<()> {
    // Armed before anything is announced, and before the first
    // synchronization pass runs. `hrc daemon` prints a startup line that a
    // supervisor, a test, or a person reads as "it is up"; if the signal
    // handlers were installed after that line, a SIGTERM arriving in the gap
    // would kill the process outright under the default disposition. The
    // daemon would have reported success and then died to the very signal it
    // advertises handling. Arming first makes the line mean what it says.
    let shutdown = arm_shutdown();
    tokio::pin!(shutdown);

    let agent_endpoint = daemon_endpoint(&context.paths, Interface::AgentSafe)?;
    let trusted_endpoint = daemon_endpoint(&context.paths, Interface::TrustedHuman)?;
    let agent_context = context.clone();
    let trusted_context = context.clone();
    let loop_context = context.clone();
    let context_authorizations =
        Arc::new(Mutex::new(hrc_core::rpc::ContextAuthorizationLedger::new()));
    let trusted_context_authorizations = Arc::clone(&context_authorizations);

    // The first synchronization pass, whose result is the startup line. It
    // runs here rather than before the runtime so that the daemon is already
    // stoppable while it happens: on a channel with a slow remote this pass
    // is the longest part of starting up.
    announce(daemon_tick(context)?)?;

    let served = async {
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
                    handle_trusted_request(
                        &trusted_context,
                        &trusted_context_authorizations,
                        request,
                    )
                })
                .await
                .map_err(CliError::from)
            },
            daemon_sync_loop(&loop_context),
        )
    };
    tokio::pin!(served);

    let outcome = tokio::select! {
        // Biased so a shutdown that arrives at the same moment as an error
        // is still a shutdown. Reporting a failure caused by tearing the
        // daemon down would make a clean stop look like a crash.
        biased;

        () = &mut shutdown => {
            // The listeners stop accepting when this future is dropped, and
            // an in-flight synchronization tick is synchronous, so it has
            // already finished or has not started.
            Ok(())
        }
        result = &mut served => result.map(|_| ()),
    };

    // Endpoints are removed on the way out rather than left for the next
    // start to probe and reclaim. On Unix a stale socket file makes the next
    // bind do extra work to prove nobody is behind it; on Windows the pipe
    // name simply goes when the process does.
    release_endpoint(&agent_endpoint);
    release_endpoint(&trusted_endpoint);

    outcome
}

/// Resolves when the operating system asks this process to stop.
///
/// Ctrl-C everywhere, and SIGTERM as well on Unix, because that is what a
/// service manager and a container runtime send. A daemon that handled only
/// Ctrl-C would shut down cleanly when a developer stopped it by hand and be
/// killed outright by every automated stop, which is the case that matters.
#[cfg(unix)]
fn arm_shutdown() -> impl std::future::Future<Output = ()> {
    use tokio::signal::unix::{SignalKind, signal};

    // Registered here rather than inside the returned future, and this is
    // the whole point of splitting "arm" from "wait": an `async fn` body
    // does not run until something polls it, so a handler installed there
    // would not exist until the select loop first ran. Installing it while
    // building the future means the process stops trusting the default
    // disposition from this line onward.
    //
    // Without SIGTERM handling, Ctrl-C alone is still better than nothing,
    // so a failure to register degrades rather than refusing to start.
    let terminate = signal(SignalKind::terminate()).ok();

    async move {
        match terminate {
            Some(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => {}
                    _ = terminate.recv() => {}
                }
            }
            None => {
                let _ = tokio::signal::ctrl_c().await;
            }
        }
    }
}

#[cfg(not(unix))]
fn arm_shutdown() -> impl std::future::Future<Output = ()> {
    async {
        let _ = tokio::signal::ctrl_c().await;
    }
}

/// Removes a local endpoint's filesystem entry, if it has one.
fn release_endpoint(endpoint: &Endpoint) {
    if let Some(path) = endpoint.path() {
        let _ = std::fs::remove_file(path);
    }
}

async fn daemon_sync_loop(context: &Context) -> Result<()> {
    loop {
        if let Err(error) = daemon_tick(context) {
            eprintln!("error: {error}");
        }

        let delay = daemon_poll_interval(context)?;
        tokio::time::sleep(delay).await;
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
        request @ Request::Agent(AgentRequest::Draft { .. })
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

fn handle_trusted_request(
    context: &Context,
    context_authorizations: &Mutex<hrc_core::rpc::ContextAuthorizationLedger>,
    request: TrustedRequest,
) -> Value {
    let method = request.method();

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

    let mut ledger = hrc_core::AuthorizationLedger::new();
    let mut context_ledger = match context_authorizations.lock() {
        Ok(ledger) => ledger,
        Err(error) => {
            return json!({
                "status": "error",
                "code": "internal_error",
                "message": format!("context authorization ledger is unavailable: {error}"),
            });
        }
    };
    let now = match broker.now() {
        Ok(now) => now,
        Err(error) => {
            return json!({
                "status": "error",
                "code": error.code(),
                "message": error.to_string(),
            });
        }
    };

    match hrc_core::rpc::dispatch_trusted(
        &mut broker,
        &mut ledger,
        &mut context_ledger,
        request,
        &now,
    ) {
        Ok(hrc_core::rpc::TrustedResponse::ContextPreview {
            package_id,
            digest,
            content,
            authorization,
            items,
            total_bytes,
        }) => json!({
            "status": "ok",
            "method": method,
            "contextId": package_id,
            "digest": digest,
            "content": content,
            "authorization": authorization,
            "items": items,
            "totalBytes": total_bytes,
        }),
        Ok(hrc_core::rpc::TrustedResponse::Delivered { agent, framed }) => json!({
            "status": "ok",
            "method": method,
            "agent": agent,
            "framed": framed,
        }),
        Ok(hrc_core::rpc::TrustedResponse::Pending { message_id, body }) => json!({
            "status": "ok",
            "method": method,
            "messageId": message_id,
            "body": body,
        }),
        Ok(hrc_core::rpc::TrustedResponse::Done) => json!({
            "status": "ok",
            "method": method,
        }),
        Err(error) => json!({
            "status": "error",
            "code": core_error_code(&error),
            "message": error.to_string(),
        }),
    }
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
    /// What each channel is called, by channel identifier, in the form every
    /// other surface shows it.
    ///
    /// The provenance banner on an approved delivery names the channel the
    /// body came from. It used to be the literal `local channel` on every
    /// delivery, because nothing here could resolve a message to its channel
    /// — the one seam no test crossed.
    channel_display_names: HashMap<String, String>,
    principal_id: String,
    device_id: String,
    checks: Vec<(String, bool)>,
    audit: Vec<String>,
    /// Pending data reconstructed from the durable, trusted-only quarantine.
    pending: Vec<hrc_core::message::QuarantinedMessage>,
    pending_bodies: HashMap<String, String>,
    /// The section 19.1 metadata set for every stored inbox row.
    ///
    /// Held as `AgentView` rather than as database rows so there is no
    /// column here that could hold a body: the agent-safe inbox answer is
    /// assembled from a type that has no field for one.
    inbox_views: Vec<hrc_core::gate::AgentView>,
    /// Content a human already released, keyed by message.
    ///
    /// Read from the append-only decision record rather than from the inbox
    /// row, because the decision is what a human authorized. A row whose
    /// disposition says `approved` with no decision behind it releases
    /// nothing.
    approved: HashMap<String, String>,
    /// Where the database lives, so a decision can be appended durably
    /// rather than held in memory until the daemon exits.
    database_path: std::path::PathBuf,
    /// Everything a trusted operation needs to publish a control entry.
    ///
    /// The broker performs membership changes itself rather than handing a
    /// plan back to a caller. A trusted operation that returned instructions
    /// would put the decision and its execution in two places, and only one
    /// of them is behind the human interface.
    context: Context,
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
        let channel_display_names = channel_records
            .iter()
            .map(|channel| {
                (
                    channel.channel_id.clone(),
                    hrc_herdr::channel_display_name(&channel.local_name),
                )
            })
            .collect();

        let store = context.key_store()?;
        let secrets = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
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
        let pending_rows = database.pending_inbound()?;
        let pending_bodies = pending_rows
            .iter()
            .map(|row| (row.message_id.clone(), row.body.clone()))
            .collect();
        let pending = pending_rows
            .into_iter()
            .map(|row| {
                Ok(hrc_core::message::QuarantinedMessage {
                    envelope: MessageEnvelope {
                        version: hrc_protocol::PROTOCOL_VERSION,
                        channel_id: row.channel_id,
                        roster_epoch: row.roster_epoch,
                        message_id: row.message_id,
                        device_sequence: 0,
                        previous_chain_id: None,
                        created_at: row.created_at,
                        expires_at: row.expires_at,
                        to: hrc_protocol::Addressing {
                            principals: Vec::new(),
                            endpoint: row.endpoint,
                        },
                        recipients: hrc_protocol::RecipientDevices::new([row
                            .sender_device
                            .clone()])?,
                        recipient_previous_chain_ids: None,
                        thread_id: String::new(),
                        in_reply_to: None,
                        kind: row.kind,
                        requested_capability: None,
                        body: Value::Null,
                        attachments: Vec::new(),
                        padding: String::new(),
                    },
                    sender_principal: row.sender_principal,
                    sender_device: row.sender_device,
                    ciphertext_sha256: row.ciphertext_sha256,
                })
            })
            .collect::<Result<Vec<_>>>()?;

        let mut inbox_views = Vec::new();
        let mut approved = HashMap::new();
        for channel in &channel_records {
            for entry in database.plugin_inbox(&channel.channel_id)? {
                // No local alias store exists yet, so the verified principal
                // ID stands in for the display name, exactly as the plugin
                // pane does.
                inbox_views.push(hrc_herdr::agent_view(
                    &entry,
                    &entry.sender_principal,
                    &channel.local_name,
                ));

                if let Some(text) = released_content(&database, &entry.message_id)? {
                    approved.insert(entry.message_id.clone(), text);
                }
            }
        }

        Ok(Self {
            channel_status,
            channel_names,
            channel_display_names,
            principal_id,
            device_id,
            checks,
            audit,
            pending,
            pending_bodies,
            inbox_views,
            approved,
            database_path: context.paths.database(),
            context: context.clone(),
        })
    }
}

/// The content a human released for one message, if they released any.
///
/// Only a decision that actually delivers produces readable content.
/// Keeping a message in the inbox and declining it are decisions too, and
/// neither authorizes disclosure — so this returns `None` for both, and the
/// agent-safe surface cannot tell them apart from a message that was never
/// decided at all.
fn released_content(database: &Database, message_id: &str) -> Result<Option<String>> {
    let Some(decision) = database
        .decisions_for(message_id)?
        .into_iter()
        .rfind(|decision| {
            matches!(
                decision.action.as_str(),
                "deliver_to_agent" | "deliver_edited"
            )
        })
    else {
        return Ok(None);
    };

    // The edit is what the human approved when they made one. Returning the
    // original in that case would hand an agent text a human chose not to
    // send it.
    let body = decision
        .edited_content
        .clone()
        .unwrap_or_else(|| decision.original_content.clone());

    let channel_local_name = database
        .channel(&decision.channel_id)?
        .map(|channel| channel.local_name)
        .unwrap_or_else(|| decision.channel_id.clone());

    let sender_principal = database
        .inbox_entries(&decision.channel_id)?
        .into_iter()
        .find(|entry| entry.message_id == message_id)
        .map(|entry| entry.sender_principal)
        .unwrap_or_default();

    // Reframed rather than stored framed: the banner names the channel and
    // the approver as they are known now, and a banner kept as text would be
    // one more place a sender-influenced string could be edited into.
    let banner = hrc_core::gate::provenance_banner(
        &sender_principal,
        &channel_local_name,
        message_id,
        &decision.decided_by,
    );

    Ok(Some(format!("{banner}{body}")))
}

impl DaemonBroker {
    /// The local clock, from the database so every record agrees on it.
    fn now(&self) -> Result<String> {
        Ok(Database::open(&self.database_path)?.utc_now()?)
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

    fn inbox(&self, pending_only: bool) -> Vec<hrc_core::gate::AgentView> {
        // Built from the same stored row and the same mapping the Herdr
        // plugin uses, so the daemon's answer and the plugin's pane cannot
        // disagree about what section 19.1 admits.
        self.inbox_views
            .iter()
            .filter(|view| !pending_only || view.awaiting_decision)
            .cloned()
            .collect()
    }

    fn approved_content(&self, message_id: &str) -> Option<String> {
        self.approved.get(message_id).cloned()
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

    fn preview_context(
        &self,
        package_id: &str,
    ) -> hrc_core::Result<hrc_core::rpc::ContextPreviewSummary> {
        let (package, digest, repository_root) =
            load_context_draft(&self.context, package_id).map_err(context_core_error)?;
        let mut preview = package.preview()?;
        append_repository_exclusions(&package, repository_root.as_deref(), &mut preview)
            .map_err(context_core_error)?;
        verify_excerpt_sources(&package, repository_root.as_deref()).map_err(context_core_error)?;
        if !preview.is_sendable() {
            return Err(hrc_core::CoreError::MalformedMessage {
                reason: "context package is blocked by source or secret checks".into(),
            });
        }
        let content =
            String::from_utf8_lossy(&hrc_protocol::canonical::to_canonical_bytes(&package)?)
                .into_owned();
        Ok(hrc_core::rpc::ContextPreviewSummary {
            package_id: package.id,
            digest,
            content,
            items: preview.items,
            total_bytes: preview.total_bytes,
        })
    }

    fn context_authorization_scope(
        &self,
        package_id: &str,
        recipient: &str,
    ) -> hrc_core::Result<hrc_core::rpc::ContextAuthorizationScope> {
        let (_, digest, _) =
            load_context_draft(&self.context, package_id).map_err(context_core_error)?;
        let database = Database::open(&self.database_path)
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;
        let channel = only_channel(&database).map_err(context_core_error)?;
        Ok(hrc_core::rpc::ContextAuthorizationScope {
            channel_id: channel.channel_id,
            package_id: package_id.to_owned(),
            digest,
            recipient: recipient.to_owned(),
            action: "send_context".into(),
        })
    }

    fn context_authorization_expiry(&self, now: &str) -> hrc_core::Result<String> {
        expiry_from(now, "5m").map_err(context_core_error)
    }

    fn send_context(
        &mut self,
        scope: &hrc_core::rpc::ContextAuthorizationScope,
    ) -> hrc_core::Result<()> {
        let (_, digest, _) =
            load_context_draft(&self.context, &scope.package_id).map_err(context_core_error)?;
        if digest != scope.digest {
            return Err(hrc_core::CoreError::AuthorizationMismatch);
        }
        let database = Database::open(&self.database_path)
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;
        if only_channel(&database)
            .map_err(context_core_error)?
            .channel_id
            != scope.channel_id
        {
            return Err(hrc_core::CoreError::AuthorizationMismatch);
        }
        context_send(&self.context, &scope.recipient, &scope.package_id)
            .map(|_| ())
            .map_err(context_core_error)
    }

    fn pending(&self, message_id: &str) -> Option<&hrc_core::message::QuarantinedMessage> {
        self.pending
            .iter()
            .find(|message| message.envelope.message_id == message_id)
    }

    fn pending_body(&self, message_id: &str) -> Option<String> {
        self.pending_bodies.get(message_id).cloned()
    }

    fn channel_local_name(&self, message_id: &str) -> String {
        // Resolved from the quarantined message itself, whose channel was
        // verified when it arrived, rather than from anything a caller
        // supplied. A message this broker does not hold cannot be delivered
        // at all, so the fallback is a fixed local phrase that says so rather
        // than a guess at which channel it might have been.
        self.pending(message_id)
            .and_then(|message| {
                self.channel_display_names
                    .get(&message.envelope.channel_id)
                    .cloned()
            })
            .unwrap_or_else(|| "an unknown channel".to_owned())
    }

    fn local_user(&self) -> String {
        std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .unwrap_or_else(|_| "local human".into())
    }

    fn record_decision_record(
        &mut self,
        record: &hrc_core::DecisionRecord,
    ) -> hrc_core::Result<()> {
        // Written through to the append-only audit log rather than kept in
        // memory: a decision that a restart erased would not be evidence of
        // anything (PRD requirement HRC-GATE-004).
        let mut database = Database::open(&self.database_path)
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

        database
            .commit_decision(&hrc_storage::DecisionRecord {
                channel_id: record.channel_id.clone(),
                message_id: record.message_id.clone(),
                action: record.action.to_owned(),
                original_content: record.original_content.clone(),
                edited_content: record.edited_content.clone(),
                content_hash: record.content_hash.clone(),
                edited_hash: record.edited_hash.clone(),
                agent: record.agent.clone(),
                decided_by: record.decided_by.clone(),
                occurred_at: record.occurred_at.clone(),
            })
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

        Ok(())
    }

    fn publication_channel_name(&self) -> hrc_core::Result<String> {
        let database = Database::open(self.context.paths.database())
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;
        let channel = only_channel(&database)
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

        Ok(channel.local_name)
    }

    fn make_repository_public(
        &mut self,
        confirmed: &hrc_core::visibility::ConfirmedPublication,
    ) -> hrc_core::Result<()> {
        make_repository_public(&self.context, confirmed)
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))
    }

    fn apply_trusted(&mut self, request: &TrustedRequest) -> hrc_core::Result<()> {
        let operation = match request {
            TrustedRequest::RemoveMember { principal_id } => ControlOperation::RemoveMember {
                principal_id: principal_id.clone(),
            },
            TrustedRequest::RevokeDevice { device_id } => ControlOperation::RevokeDevice {
                device_id: device_id.clone(),
            },
            TrustedRequest::ApproveJoin { request_id } => {
                return admit_join(&self.context, request_id)
                    .map_err(|error| hrc_core::CoreError::Transport(error.to_string()));
            }

            // Declining a join publishes nothing. A refusal that left a
            // record in the channel would tell everyone who was turned away,
            // which is the administrator's business and not the channel's.
            TrustedRequest::RejectJoin { .. } => return Ok(()),

            // Local only. An alias is what *this* installation calls
            // someone; publishing it would tell the channel what its members
            // think of each other, and would let a name one person chose
            // reach a screen belonging to someone who did not choose it.
            TrustedRequest::SetAlias {
                principal_id,
                display_name,
            } => {
                let database = Database::open(self.context.paths.database())
                    .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;
                let channel = only_channel(&database)
                    .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;
                let now = database
                    .utc_now()
                    .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

                match display_name {
                    Some(display_name) => database.set_principal_alias(
                        &channel.channel_id,
                        principal_id,
                        display_name,
                        &now,
                    ),
                    None => database
                        .clear_principal_alias(&channel.channel_id, principal_id)
                        .map(|_| ()),
                }
                .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

                return Ok(());
            }

            // The rest are not membership changes and have no control entry.
            _ => return Ok(()),
        };

        publish_membership_change(&self.context, operation)
            .map(|_| ())
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))
    }
}

/// The trusted daemon boundary already hides detailed context source errors
/// from agent-safe callers. Preserve that boundary when the CLI helpers are
/// called through the trusted RPC implementation.
fn context_core_error(error: CliError) -> hrc_core::CoreError {
    hrc_core::CoreError::Transport(error.to_string())
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
    context: &Context,
    database: &mut Database,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<Value> {
    if let Some(reason) = &channel.halted_reason {
        return Err(hrc_core::CoreError::SynchronizationHalted {
            reason: reason.clone(),
        }
        .into());
    }

    if channel.transport_kind != "git" {
        return Err(CliError::Io {
            action: "open the configured transport",
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("unsupported transport {}", channel.transport_kind),
            ),
        });
    }

    let git_dir = context.paths.channel_transport(&channel.channel_id);
    if let Some(parent) = git_dir.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CliError::Io {
            action: "create the local transport directory",
            source,
        })?;
    }

    let mut transport = GitTransport::open(&git_dir, &channel.transport_locator)?;
    let remote_head = transport.remote_head()?;
    let local_head = match transport.open_group() {
        Ok(group) => group.revision,
        Err(hrc_transport::TransportError::NoSuchGroup) => None,
        Err(error) => return Err(error.into()),
    };
    if remote_head.is_none() {
        let error = hrc_core::CoreError::Transport(
            "the remote channel branch disappeared after registration".into(),
        );
        return Err(hrc_core::sync::halt_synchronization(
            database,
            &channel.channel_id,
            error,
            now,
        )
        .into());
    }

    let remote_changed = remote_head != channel.sync_cursor;
    let recovered = database.recover_reservations(&channel.channel_id, now)?;

    let local_out_of_sync = local_head != remote_head;
    if remote_changed || local_out_of_sync {
        transport.sync_from_remote()?;
    }

    if remote_changed || local_out_of_sync {
        validate_trusted_cursor(&transport, database, channel, now)?;
    }

    let received = if channel.receive_cursor != remote_head {
        receive_for(context, channel, now).map_err(|error| {
            halt_if_received_history_is_invalid(database, &channel.channel_id, error, now)
        })?
    } else {
        Vec::new()
    };

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

    let device = match context.passphrase.clone() {
        Some(passphrase) => {
            let store = PassphraseStore::open(context.paths.keys(), passphrase)?;
            if store.contains(DEVICE_KEY_NAME)? {
                Some(store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?)
            } else {
                None
            }
        }
        None => None,
    };

    let published = publish_pending_outgoing(
        &mut transport,
        database,
        &channel.channel_id,
        device.as_ref(),
        3,
        now,
    )?;

    let receipt_error = if context.passphrase.is_some() {
        match publish_delivery_receipts(context, channel, now) {
            Ok(()) => None,
            Err(error) => {
                let detail = error.to_string();
                database.append_audit(
                    Some(&channel.channel_id),
                    None,
                    "delivery_receipt_deferred",
                    None,
                    Some(&detail),
                    now,
                )?;
                Some(detail)
            }
        }
    } else {
        None
    };
    let final_cursor = transport.open_group()?.revision;
    if let Some(cursor) = final_cursor.as_deref() {
        // Locally published commits are trusted history too. Recording the
        // final local head makes a later remote rollback of our own message
        // or receipt visible even when the remote lands exactly on the
        // cursor that preceded that publication.
        database.set_sync_cursor(&channel.channel_id, cursor)?;
    }

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
        "receivedMessages": received,
        "receiptError": receipt_error,
        "cursor": final_cursor.or_else(|| fetch.and_then(|outcome| outcome.cursor)),
    }))
}

fn publish_pending_outgoing(
    transport: &mut GitTransport,
    database: &mut Database,
    channel_id: &str,
    device: Option<&DeviceSecrets>,
    max_attempts: u32,
    now: &str,
) -> Result<Vec<hrc_core::sync::PublishOutcome>> {
    let mut blocked_devices = std::collections::HashSet::new();
    let mut outcomes = Vec::new();

    for message_id in database.pending_outgoing(channel_id)? {
        let Some(outgoing) = database.pending_outgoing_record(&message_id)? else {
            continue;
        };
        if blocked_devices.contains(&outgoing.device_id) {
            outcomes.push(hrc_core::sync::PublishOutcome {
                published: Vec::new(),
                conflicts: 0,
                deferred: vec![outgoing.message_id],
            });
            continue;
        }

        let device_id = outgoing.device_id.clone();
        let outcome = publish_outgoing(
            transport,
            database,
            channel_id,
            outgoing,
            device,
            max_attempts,
            now,
        )?;
        if !outcome.deferred.is_empty() {
            blocked_devices.insert(device_id);
        }
        outcomes.push(outcome);
    }

    Ok(outcomes)
}

fn publish_outgoing(
    transport: &mut GitTransport,
    database: &mut Database,
    channel_id: &str,
    mut outgoing: PendingOutgoing,
    device: Option<&DeviceSecrets>,
    max_attempts: u32,
    now: &str,
) -> Result<hrc_core::sync::PublishOutcome> {
    let message_id = outgoing.message_id.clone();
    let object_name =
        outgoing_message_object(&outgoing.message_id, &outgoing.created_at, Vec::new()).name;
    let mut known_ciphertexts = vec![outgoing.ciphertext.clone()];
    Ok(hrc_core::sync::publish_one_prepared(
        transport,
        database,
        channel_id,
        &message_id,
        |transport, database| {
            let channel = database.channel(channel_id)?.ok_or_else(|| {
                hrc_core::CoreError::UnknownChannelState {
                    channel_id: channel_id.to_owned(),
                }
            })?;
            validate_trusted_cursor(transport, database, &channel, now)
                .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

            let mut object_exists = false;
            for ciphertext in &known_ciphertexts {
                let expected = canonical::sha256_hex(ciphertext);
                match transport.get_object(&object_name, &expected) {
                    Ok(_) => {
                        return Ok(hrc_core::sync::PreparedPublication::AlreadyPublished);
                    }
                    Err(hrc_transport::TransportError::NoSuchObject { .. }) => {
                        object_exists = false;
                        break;
                    }
                    Err(hrc_transport::TransportError::ObjectHashMismatch { .. }) => {
                        object_exists = true;
                    }
                    Err(error) => {
                        return Err(hrc_core::CoreError::Transport(error.to_string()));
                    }
                }
            }
            if object_exists {
                return Err(hrc_core::sync::halt_synchronization(
                    database,
                    channel_id,
                    hrc_core::CoreError::Transport(
                        hrc_transport::TransportError::ObjectHashMismatch {
                            name: object_name.clone(),
                        }
                        .to_string(),
                    ),
                    now,
                ));
            }

            let (roster, _) = load_roster(transport, channel_id).map_err(|error| {
                hrc_core::sync::halt_synchronization(
                    database,
                    channel_id,
                    hrc_core::CoreError::Transport(error.to_string()),
                    now,
                )
            })?;

            if outgoing.roster_epoch > roster.epoch() {
                return Err(hrc_core::CoreError::EpochOutOfOrder {
                    expected: roster.epoch(),
                    found: outgoing.roster_epoch,
                });
            }

            if outgoing.roster_epoch < roster.epoch() || outgoing.recipient_order_stale {
                let Some(device) = device else {
                    return Ok(hrc_core::sync::PreparedPublication::Defer);
                };
                if outgoing.reseal_material.is_none() {
                    database.record_attempt_failure(
                        &outgoing.message_id,
                        "queued message predates re-encryption support and cannot be resealed",
                        now,
                    )?;
                    return Ok(hrc_core::sync::PreparedPublication::Defer);
                }

                let material = outgoing.reseal_material.as_deref().ok_or_else(|| {
                    hrc_core::CoreError::OutboxMessageMismatch {
                        message_id: outgoing.message_id.clone(),
                        field: "re-encryption material",
                    }
                })?;
                let identity = device.device_identity()?;
                let plaintext = identity.decrypt(material)?;
                let envelope: MessageEnvelope =
                    canonical::from_json_str(std::str::from_utf8(&plaintext).map_err(|_| {
                        hrc_core::CoreError::MalformedMessage {
                            reason: "protected outbox material is not UTF-8".into(),
                        }
                    })?)?;
                let recipients = hrc_core::message::intended_recipients(&roster, &envelope)?
                    .into_iter()
                    .map(|recipient| recipient.device_id.clone())
                    .collect::<Vec<_>>();
                let mut replacement = None;
                database.reseal_outgoing(
                    &outgoing.message_id,
                    outgoing.roster_epoch,
                    roster.epoch(),
                    &recipients,
                    now,
                    |stored, predecessors| {
                        let ciphertext = hrc_core::message::reseal_outgoing_with_predecessors(
                            &roster,
                            &identity,
                            &device.signing_key(),
                            stored,
                            predecessors.clone(),
                        )?;
                        replacement = Some(ciphertext.clone());
                        Ok::<_, hrc_core::CoreError>(ciphertext)
                    },
                )?;
                let ciphertext =
                    replacement.ok_or_else(|| hrc_core::CoreError::OutboxMessageMismatch {
                        message_id: outgoing.message_id.clone(),
                        field: "replacement ciphertext",
                    })?;
                outgoing.roster_epoch = roster.epoch();
                outgoing.ciphertext = ciphertext;
                outgoing.recipient_order_stale = false;
                known_ciphertexts.push(outgoing.ciphertext.clone());
            }

            Ok(hrc_core::sync::PreparedPublication::Publish(
                outgoing_message_object(
                    &outgoing.message_id,
                    &outgoing.created_at,
                    outgoing.ciphertext.clone(),
                ),
            ))
        },
        max_attempts,
        now,
    )?)
}

fn validate_trusted_cursor(
    transport: &GitTransport,
    database: &Database,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<()> {
    let Some(cursor) = channel.sync_cursor.as_deref() else {
        return Ok(());
    };

    transport
        .fetch(Some(cursor), 1)
        .map(|_| ())
        .map_err(|error| {
            let error = hrc_core::CoreError::Transport(error.to_string());
            hrc_core::sync::halt_synchronization(database, &channel.channel_id, error, now).into()
        })
}

fn halt_if_received_history_is_invalid(
    database: &Database,
    channel_id: &str,
    error: CliError,
    now: &str,
) -> CliError {
    match error {
        CliError::Core(_) | CliError::Protocol(_) | CliError::Transport(_) => {
            let error = hrc_core::CoreError::Transport(error.to_string());
            hrc_core::sync::halt_synchronization(database, channel_id, error, now).into()
        }
        other => other,
    }
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
fn place_inbox(context: &Context) -> Value {
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
fn placement(context: &Context, config: &hrc_herdr::Config) -> Value {
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
fn inbox_pane_record() -> Option<std::path::PathBuf> {
    let directory = std::env::var(hrc_herdr::host::STATE_ENV).ok()?;
    (!directory.is_empty()).then(|| std::path::Path::new(&directory).join("inbox-pane"))
}

/// The pane this plugin last placed the inbox in, if it recorded one.
fn remembered_inbox_pane() -> Option<String> {
    let recorded = std::fs::read_to_string(inbox_pane_record()?).ok()?;
    let recorded = recorded.trim();

    (!recorded.is_empty()).then(|| recorded.to_owned())
}

/// Records the pane the inbox was placed in.
///
/// A write that fails is ignored: the cost is a second inbox after a Herdr
/// handoff, and failing the startup hook over it would cost the whole plugin.
fn remember_inbox_pane(pane_id: &str) {
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
fn herdr_sidebar(context: &Context) -> Result<hrc_herdr::Sidebar> {
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

/// Makes the channel's repository publicly readable (PRD requirement
/// HRC-CH-004).
///
/// Only reachable with a [`ConfirmedPublication`], which cannot be built
/// without the full section 16.4 disclosure and the typed phrase. The audit
/// record is written *before* the change, because a publication that
/// happened and was not recorded is worse than one recorded and then
/// refused: the second is visible.
///
/// The change itself goes through `gh`, for the same reason everything else
/// does — the user's authentication stays authoritative. When `gh` cannot do
/// it, this says so and tells the human where to do it themselves rather
/// than reporting a success that did not happen.
fn make_repository_public(
    context: &Context,
    confirmed: &hrc_core::visibility::ConfirmedPublication,
) -> Result<()> {
    let database = Database::open(context.paths.database())?;
    let channel = only_channel(&database)?;
    let now = database.utc_now()?;

    database.append_audit(
        Some(&channel.channel_id),
        None,
        "repository_made_public",
        None,
        Some(&confirmed.audit_detail()),
        &now,
    )?;

    let Some(remote) = hrc_transport_git::github::GitHubRemote::parse(&channel.transport_locator)
    else {
        return Err(CliError::PublicationUnavailable {
            reason: format!(
                "{} is not a GitHub repository, so HRC cannot change its visibility. \
                 Change it with your provider; the confirmation is recorded.",
                channel.transport_locator
            ),
        });
    };

    let output = ProcessCommand::new("gh")
        .args([
            "repo",
            "edit",
            &remote.slug(),
            "--visibility",
            "public",
            "--accept-visibility-change-consequences",
        ])
        .output()
        .map_err(|error| CliError::PublicationUnavailable {
            reason: format!(
                "could not run gh ({error}). Make {} public in the GitHub interface; \
                 the confirmation is recorded.",
                remote.slug()
            ),
        })?;

    if !output.status.success() {
        return Err(CliError::PublicationUnavailable {
            reason: format!(
                "gh refused to change the visibility of {}: {}",
                remote.slug(),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        });
    }

    Ok(())
}

pub mod review;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn a_lifetime_resolves_to_an_absolute_expiry() {
        assert_eq!(
            expiry_from("2026-09-13T00:00:00Z", "24h").unwrap(),
            "2026-09-14T00:00:00Z"
        );
        assert_eq!(
            expiry_from("2026-09-13T23:30:00Z", "30m").unwrap(),
            "2026-09-14T00:00:00Z"
        );
        assert_eq!(
            expiry_from("2026-09-13T00:00:00Z", "7d").unwrap(),
            "2026-09-20T00:00:00Z"
        );
    }

    #[test]
    fn a_duration_is_never_stored_as_if_it_were_a_timestamp() {
        // The bug this replaced: "24h" compared lexicographically against a
        // date lapses at a moment that depends on the year.
        let resolved = expiry_from("2026-09-13T00:00:00Z", "24h").unwrap();

        assert!(resolved.starts_with("2026-"), "{resolved}");
        assert!(resolved.ends_with('Z'));
    }

    #[test]
    fn month_and_year_boundaries_are_handled() {
        assert_eq!(
            expiry_from("2026-01-31T12:00:00Z", "1d").unwrap(),
            "2026-02-01T12:00:00Z"
        );
        assert_eq!(
            expiry_from("2026-12-31T23:00:00Z", "2h").unwrap(),
            "2027-01-01T01:00:00Z"
        );
        // A leap year, which a naive day count gets wrong.
        assert_eq!(
            expiry_from("2028-02-28T00:00:00Z", "1d").unwrap(),
            "2028-02-29T00:00:00Z"
        );
    }

    #[test]
    fn a_timestamp_round_trips_through_both_directions() {
        for stamp in [
            "1970-01-01T00:00:00Z",
            "2000-02-29T12:34:56Z",
            "2026-09-13T07:08:09Z",
            "2100-03-01T00:00:00Z",
        ] {
            let seconds = time_from_rfc3339(stamp).expect(stamp);
            assert_eq!(rfc3339_from(seconds), stamp);
        }
    }

    #[test]
    fn a_lifetime_that_is_not_one_is_refused() {
        for bad in ["", "h", "0h", "-1h", "24", "24y", "twenty4h", "24 h"] {
            assert!(
                expiry_from("2026-09-13T00:00:00Z", bad).is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn only_the_timestamp_shape_this_project_emits_is_parsed() {
        // Accepting offsets and fractions nothing here produces would mean
        // quietly mis-parsing one of them later.
        for bad in [
            "2026-09-13T00:00:00+02:00",
            "2026-09-13T00:00:00.500Z",
            "2026-09-13 00:00:00Z",
            "2026-09-13",
        ] {
            assert!(time_from_rfc3339(bad).is_none(), "{bad:?} was parsed");
        }
    }

    #[test]
    fn an_invite_store_name_cannot_escape_the_store() {
        // Invite identifiers are base64url, but the name is built rather
        // than trusted, so this is worth pinning.
        let name = invite_store_name("iFeQifaw9tVbdMRboDaYqg");

        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
        assert!(!name.contains(".."));
        assert!(name.starts_with("invite-"));
    }
}

#[cfg(test)]
mod repository_tests {
    use super::canonical_repository;

    #[test]
    fn the_documented_shorthand_becomes_a_url_git_can_use() {
        // `--help` and PRD section 22.4 both ask for `owner/name`, and nothing
        // expanded it, so git resolved it as a relative path and the first
        // channel created from the documented form failed. Only a full URL
        // worked, which made the documentation wrong about its own CLI.
        assert_eq!(
            canonical_repository("czinegeroland/hrc-test"),
            "https://github.com/czinegeroland/hrc-test.git"
        );
        assert_eq!(
            canonical_repository("  owner/name  "),
            "https://github.com/owner/name.git"
        );
        assert_eq!(
            canonical_repository("owner/name.git"),
            "https://github.com/owner/name.git"
        );
    }

    #[test]
    fn anything_already_addressable_is_left_alone() {
        // A locator that already says where it points must survive untouched.
        // Rewriting one would silently move a channel to a different remote,
        // which is worse than refusing it.
        for locator in [
            "https://github.com/owner/name.git",
            "https://gitlab.com/owner/name.git",
            "ssh://git@github.com/owner/name.git",
            "git@github.com:owner/name.git",
        ] {
            assert_eq!(canonical_repository(locator), locator);
        }
    }

    #[test]
    fn a_local_repository_is_not_rewritten_into_a_github_url() {
        // A bare repository on disk is a legitimate transport, and the tests
        // in this crate use one. Expanding a path into a GitHub URL would
        // point a working channel at a repository nobody owns.
        for locator in [
            "/tmp/channel.git",
            "./channel.git",
            "../channel.git",
            "~/channels/one.git",
            "C:\\channels\\one.git",
            "\\\\server\\share\\one.git",
        ] {
            assert_eq!(canonical_repository(locator), locator);
        }
    }

    #[test]
    fn something_that_is_not_the_shorthand_is_passed_through() {
        // Guessing which two of three segments were meant is how a channel
        // ends up addressed to the wrong repository.
        for locator in ["owner", "owner/name/extra", "owner/", "/name"] {
            assert_eq!(canonical_repository(locator), locator);
        }
    }
}
