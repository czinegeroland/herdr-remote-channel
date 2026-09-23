//! Invites, joining, and admitting a joiner (PRD section 15).

use super::*;

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
pub(super) fn local_invite(
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

/// Admits a pending joiner, on the administrator's decision.
///
/// The request is validated again here rather than trusted from the listing:
/// the channel may have moved between the human reading a safety phrase and
/// answering, and the entry that gets published has to be built from what is
/// true now.
pub(super) fn admit_join(context: &Context, request_id: &str) -> Result<()> {
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

/// Where an invite secret is kept in the key store.
///
/// Invite identifiers are base64url, which contains `-` and `_` but no path
/// separator, so this cannot address anything outside the store directory.
pub(super) fn invite_store_name(invite_id: &str) -> String {
    format!("invite-{invite_id}")
}
