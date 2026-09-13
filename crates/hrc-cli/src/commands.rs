//! Implementations of the commands that need no network.
//!
//! Each returns a JSON value. The human renderer formats that same value, so
//! the two output modes cannot drift: there is one source of truth for what
//! a command reports, and `--json` is a formatting choice rather than a
//! separate code path.

use std::time::Duration;

use hrc_core::Roster;
use hrc_core::rpc::{AgentRequest, Broker, ChannelStatus, Request, TrustedRequest, dispatch_agent};
use hrc_core::sync::{PollActivity, poll_interval};
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
use hrc_storage::Database;
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
pub fn create(context: &Context, repo: &str, local_name: Option<&str>) -> Result<Value> {
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
fn only_channel(database: &Database) -> Result<hrc_storage::ChannelRecord> {
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

    Ok(json!({
        "status": "ok",
        "inviteId": invite_id,
        "expiresAt": expires_at,
        "intendedFor": intended_for,
        // The one place this value appears. Hand it over through a channel
        // the user chooses; it works once and then it is spent.
        "inviteCode": invite.to_code()?,
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
/// passphrase — a locked installation still keeps its queue moving, it just
/// cannot read anything.
fn receive_for(
    paths: &Paths,
    channel: &hrc_storage::ChannelRecord,
    now: &str,
) -> Result<Vec<String>> {
    let identity = match paths.stored_passphrase() {
        Some(passphrase) => {
            let store = PassphraseStore::open(paths.keys(), passphrase)?;
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
        paths.channel_transport(&channel.channel_id),
        &channel.transport_locator,
    )?;

    let mut database = Database::open(paths.database())?;
    receive_messages(&transport, &mut database, channel, identity.as_ref(), now)
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
                        let Ok(opened) =
                            hrc_core::message::open(roster, identity, &bytes, roster.epoch(), now)
                        else {
                            continue;
                        };

                        let body = opened
                            .envelope
                            .body
                            .get("text")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_owned();

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
                                kind,
                                thread_id: Some(&opened.envelope.thread_id),
                                in_reply_to: opened.envelope.in_reply_to.as_deref(),
                                roster_epoch: opened.envelope.roster_epoch,
                                endpoint: opened.envelope.to.endpoint.as_deref(),
                                ciphertext_bytes: bytes.len() as u64,
                                plaintext_bytes: body.len() as u64,
                                ciphertext_sha256: &opened.ciphertext_sha256,
                                created_at: &opened.envelope.created_at,
                                expires_at: opened.envelope.expires_at.as_deref(),
                                body: body.as_bytes(),
                                ciphertext: &bytes,
                            },
                            now,
                        )?;

                        if let hrc_storage::InboundOutcome::Accepted { released, .. } = outcome {
                            received.push(opened.envelope.message_id.clone());
                            received.extend(released);
                        }
                    }

                    _ => {}
                }
            }
        }

        if !page.more {
            break;
        }
        cursor = page.cursor;
    }

    if let Some(roster) = roster {
        database.set_roster_progress(&channel.channel_id, roster.epoch(), roster.sequence())?;
    }

    Ok(received)
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

    let body = serde_json::json!({ "text": text });
    let payload_hash = canonical::canonical_sha256_hex(&body)?;

    let reservation = database.allocate_outgoing(
        &channel.channel_id,
        &device_id,
        &message_id,
        roster.epoch(),
        &payload_hash,
        &now,
    )?;

    let envelope = MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: channel.channel_id.clone(),
        roster_epoch: roster.epoch(),
        message_id: message_id.clone(),
        device_sequence: reservation.device_sequence,
        previous_chain_id: reservation.previous_chain_id.clone(),
        created_at: now.clone(),
        expires_at: None,
        to: hrc_protocol::Addressing {
            principals: vec![principal_id.to_owned()],
            endpoint,
        },
        recipients: hrc_protocol::RecipientDevices::new([device_id.clone()])?,
        thread_id: thread_id.clone(),
        in_reply_to,
        kind: kind.as_str().to_owned(),
        requested_capability: None,
        body,
        attachments: Vec::new(),
        padding: String::new(),
    };

    let ciphertext = hrc_core::message::seal(
        &roster,
        &device.signing_key(),
        Signer {
            principal_id: principal.signing_key().verifying_key().to_base64url(),
            device_id: device_id.clone(),
        },
        envelope,
    )?;

    database.queue_outgoing(&message_id, &ciphertext, &now)?;

    let outcome = hrc_core::sync::publish_one(
        &mut transport,
        &database,
        &channel.channel_id,
        &message_id,
        PublishObject {
            name: message_object_name(&now, &message_id),
            class: ObjectClass::Message,
            bytes: ciphertext,
        },
        5,
        &now,
    )?;

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
        "published": !outcome.published.is_empty(),
        "deferred": !outcome.deferred.is_empty(),
    }))
}

/// `hrc send`: an informational note.
pub fn send(context: &Context, recipient: &str, text: &str) -> Result<Value> {
    compose(
        context,
        hrc_protocol::MessageKind::Note,
        recipient,
        text,
        None,
    )
}

/// `hrc ask`: a question, optionally addressed to a logical endpoint.
pub fn ask(context: &Context, recipient: &str, text: &str) -> Result<Value> {
    compose(
        context,
        hrc_protocol::MessageKind::Question,
        recipient,
        text,
        None,
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
    )
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

/// Where a message object lives in the channel layout (PRD section 16.2).
fn message_object_name(now: &str, message_id: &str) -> String {
    let year = now.get(0..4).unwrap_or("0000");
    let month = now.get(5..7).unwrap_or("00");

    format!("messages/{year}/{month}/{message_id}.age")
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
fn rfc3339_from(seconds: i64) -> String {
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

fn handle_trusted_request(context: &Context, request: TrustedRequest) -> Value {
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

    // A fresh ledger per connection is deliberate for now: an authorization
    // is issued, consumed, and discarded inside one call (DEC-040), so
    // nothing needs to outlive the request that created it.
    let mut ledger = hrc_core::AuthorizationLedger::new();
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

    match hrc_core::rpc::dispatch_trusted(&mut broker, &mut ledger, request, &now) {
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
    principal_id: String,
    device_id: String,
    checks: Vec<(String, bool)>,
    audit: Vec<String>,
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

        Ok(Self {
            channel_status,
            channel_names,
            principal_id,
            device_id,
            checks,
            audit,
            database_path: context.paths.database(),
            context: context.clone(),
        })
    }
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

    fn record_decision_record(
        &mut self,
        record: &hrc_core::DecisionRecord,
    ) -> hrc_core::Result<()> {
        // Written through to the append-only audit log rather than kept in
        // memory: a decision that a restart erased would not be evidence of
        // anything (PRD requirement HRC-GATE-004).
        let database = Database::open(&self.database_path)
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))?;

        database
            .append_decision(&hrc_storage::DecisionRecord {
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

            // The rest are not membership changes and have no control entry.
            _ => return Ok(()),
        };

        publish_membership_change(&self.context, operation)
            .map(|_| ())
            .map_err(|error| hrc_core::CoreError::Transport(error.to_string()))
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

    if remote_changed || (remote_head.is_some() && local_missing) {
        transport.sync_from_remote()?;
    }

    if remote_changed {
        validate_trusted_cursor(&transport, database, channel, now)?;
    }

    let received = if remote_changed || local_missing {
        receive_for(paths, channel, now).map_err(|error| {
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
        "receivedMessages": received,
        "cursor": fetch.and_then(|outcome| outcome.cursor),
    }))
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
