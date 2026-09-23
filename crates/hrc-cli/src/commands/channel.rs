//! Creating a channel, reading and extending its control history, and
//! making its repository public.

use super::*;

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
pub(super) fn canonical_repository(repo: &str) -> String {
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
pub(super) fn load_roster(
    transport: &GitTransport,
    channel_id: &str,
) -> Result<(Roster, Revision)> {
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
pub(super) fn publish_control(
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
pub(super) fn make_repository_public(
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
