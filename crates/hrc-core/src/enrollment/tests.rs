//! Enrollment tests.
//!
//! Enrollment is where a stranger first gets to hand the channel an object,
//! so these lean on the ways that object could lie: a proof made for someone
//! else, a certificate swapped after the proof was computed, a request
//! replayed after it was already honored, an invite that the control log
//! never opened or has already spent.
//!
//! Everything here uses real keys, real age encryption, and a real control
//! chain. A stub would happily accept exactly the forgeries under test.

use super::*;

use hrc_crypto::enrollment::Invite;
use hrc_crypto::{DeviceIdentity, SigningKey};
use hrc_protocol::control::{GenesisPayload, TransportLocator};
use hrc_protocol::identity::{DeviceCertificatePayload, DeviceDescriptor};

const NOW: &str = "2026-09-13T02:00:00Z";
const EXPIRES: &str = "2026-09-14T00:00:00Z";
const AFTER_EXPIRY: &str = "2026-09-15T00:00:00Z";

/// A principal with one device, its age identity, and both signing keys.
struct TestPrincipal {
    id: String,
    principal_key: SigningKey,
    device_key: SigningKey,
    identity: DeviceIdentity,
    certificate: DeviceCertificate,
}

impl TestPrincipal {
    fn new(id: &str, seed: u8) -> Self {
        let principal_key = SigningKey::from_seed(&[seed; 32]);
        let device_key = SigningKey::from_seed(&[seed.wrapping_add(100); 32]);
        let identity = DeviceIdentity::generate();

        let certificate = certify(&principal_key, id, &device_key, &identity, seed);

        Self {
            id: id.to_owned(),
            principal_key,
            device_key,
            identity,
            certificate,
        }
    }

    fn material(&self) -> PrincipalMaterial {
        PrincipalMaterial {
            principal_id: self.id.clone(),
            principal_signing_key: self.principal_key.verifying_key().to_base64url(),
            devices: vec![self.certificate.clone()],
        }
    }

    fn signer(&self) -> Signer {
        Signer {
            principal_id: self.id.clone(),
            device_id: self.certificate.payload.device_id.clone(),
        }
    }
}

/// Builds a device certificate signed by a principal key.
fn certify(
    principal_key: &SigningKey,
    principal_id: &str,
    device_key: &SigningKey,
    identity: &DeviceIdentity,
    seed: u8,
) -> DeviceCertificate {
    let descriptor = DeviceDescriptor {
        version: hrc_protocol::PROTOCOL_VERSION,
        principal_id: principal_id.to_owned(),
        encryption_recipient: identity.recipient().to_string(),
        signing_key: device_key.verifying_key().to_base64url(),
        created_at: "2026-09-13T00:00:00Z".into(),
        expires_at: None,
        nonce: canonical::encode_base64url(&[seed; 16]),
    };

    let payload = DeviceCertificatePayload::new(descriptor).unwrap();
    principal_key
        .sign_object(
            domain::DEVICE_CERTIFICATE,
            Signer {
                principal_id: principal_id.to_owned(),
                device_id: payload.device_id.clone(),
            },
            payload,
        )
        .unwrap()
}

/// A channel with one administrator and one open invite.
fn channel() -> (TestPrincipal, TestPrincipal, Roster, Invite) {
    let admin = TestPrincipal::new("admin", 1);
    let joiner = TestPrincipal::new("joiner", 2);

    let genesis_payload = GenesisPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        created_at: "2026-09-13T00:00:00Z".into(),
        initial_admin: admin.material(),
        transport: TransportLocator {
            kind: "git".into(),
            locator: "owner/channel".into(),
        },
        policy: serde_json::json!({}),
    };
    let genesis = admin
        .device_key
        .sign_object(domain::GENESIS, admin.signer(), genesis_payload)
        .unwrap();

    let mut roster = Roster::from_genesis(&genesis).unwrap();
    let invite = Invite::generate(
        "owner/channel",
        roster.channel_id(),
        "admin-fingerprint",
        "invite-1",
        EXPIRES,
    )
    .unwrap();

    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::CreateInvite {
            invite_id: invite.invite_id.clone(),
            expires_at: invite.expires_at.clone(),
        },
    );
    roster.apply(&entry).unwrap();

    (admin, joiner, roster, invite)
}

/// Signs the next control entry as the administrator.
fn control_entry(
    roster: &Roster,
    admin: &TestPrincipal,
    operation: ControlOperation,
) -> SignedObject<ControlEntryPayload> {
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
        created_at: NOW.into(),
        operation,
    };

    admin
        .device_key
        .sign_object(domain::CONTROL, admin.signer(), payload)
        .unwrap()
}

/// The joiner's request for the standard channel.
fn request(roster: &Roster, invite: &Invite, joiner: &TestPrincipal) -> Vec<u8> {
    request_join(
        roster,
        invite,
        &joiner.principal_key,
        &joiner.id,
        joiner.certificate.clone(),
        NOW,
    )
    .unwrap()
}

#[test]
fn a_joiner_is_reviewed_and_admitted_end_to_end() {
    let (admin, joiner, mut roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();

    assert_eq!(pending.principal_id, "joiner");
    assert_eq!(pending.invite_id, "invite-1");
    assert_eq!(pending.device_id(), joiner.certificate.payload.device_id);

    let entry = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();
    roster.apply(&entry).unwrap();

    assert_eq!(roster.epoch(), 1);
    assert!(
        roster
            .members()
            .any(|member| member.principal_id == "joiner" && member.is_active)
    );
    assert_eq!(roster.active_devices().count(), 2);
    assert_eq!(
        roster.invite("invite-1"),
        Some(&InviteState::Consumed {
            principal_id: "joiner".into()
        })
    );
}

#[test]
fn both_sides_derive_the_same_safety_phrase() {
    // The phrase is the only defense against a substituted key, so the two
    // machines must agree without ever exchanging it.
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();
    let joiner_side = joiner_safety_phrase(
        &roster,
        &invite,
        &joiner.principal_key.verifying_key().to_base64url(),
        &joiner.certificate,
    )
    .unwrap();

    assert_eq!(pending.safety_phrase, joiner_side);
    assert_eq!(pending.safety_phrase.words.len(), 6);
}

#[test]
fn a_substituted_joiner_key_changes_the_phrase() {
    // What the humans are actually detecting when they compare.
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);
    let honest = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();

    let attacker = TestPrincipal::new("joiner", 9);
    let impostor = joiner_safety_phrase(
        &roster,
        &invite,
        &attacker.principal_key.verifying_key().to_base64url(),
        &attacker.certificate,
    )
    .unwrap();

    assert_ne!(honest.safety_phrase, impostor);
    let _ = joiner;
}

#[test]
fn a_replayed_request_cannot_be_admitted_twice() {
    // PRD section 28: the replayed join request. The first admission spends
    // the invite, and the control log is what records that, so the replay
    // fails for every participant rather than only for the administrator who
    // happens to remember.
    let (admin, joiner, mut roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();
    let entry = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();
    roster.apply(&entry).unwrap();

    // Replaying the identical ciphertext no longer reviews.
    let error = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap_err();
    assert!(matches!(error, CoreError::InviteAlreadyUsed { .. }));

    // And even an administrator who tried to admit the stale proposal again
    // is rejected by roster replay.
    let replay = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();
    assert!(matches!(
        roster.apply(&replay).unwrap_err(),
        CoreError::InviteAlreadyUsed { .. }
    ));
}

#[test]
fn an_admission_naming_an_invite_the_log_never_opened_is_rejected() {
    // Otherwise an administrator could admit anyone and the record would not
    // show which authorization it happened under.
    let (admin, joiner, mut roster, _invite) = channel();

    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::AddMember {
            invite_id: "never-created".into(),
            member: joiner.material(),
        },
    );

    assert!(matches!(
        roster.apply(&entry).unwrap_err(),
        CoreError::UnknownInvite { .. }
    ));
}

#[test]
fn a_revoked_invite_admits_nobody() {
    let (admin, joiner, mut roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let revoke = control_entry(
        &roster,
        &admin,
        ControlOperation::RevokeInvite {
            invite_id: invite.invite_id.clone(),
        },
    );
    roster.apply(&revoke).unwrap();

    let error = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap_err();
    assert!(matches!(error, CoreError::InviteRevoked { .. }));

    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::AddMember {
            invite_id: invite.invite_id.clone(),
            member: joiner.material(),
        },
    );
    assert!(matches!(
        roster.apply(&entry).unwrap_err(),
        CoreError::InviteRevoked { .. }
    ));
}

#[test]
fn an_expired_invite_is_refused_on_both_sides() {
    let (admin, joiner, roster, invite) = channel();

    let error = request_join(
        &roster,
        &invite,
        &joiner.principal_key,
        &joiner.id,
        joiner.certificate.clone(),
        AFTER_EXPIRY,
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::InviteExpired { .. }));

    // A request made in time is still refused once the invite lapses: the
    // administrator's clock decides, not the joiner's.
    let ciphertext = request(&roster, &invite, &joiner);
    let error =
        review_join(&roster, &invite, &admin.identity, &ciphertext, AFTER_EXPIRY).unwrap_err();
    assert!(matches!(error, CoreError::InviteExpired { .. }));
}

#[test]
fn a_proof_made_for_a_different_invite_does_not_transfer() {
    let (admin, joiner, mut roster, invite) = channel();

    let other = Invite::generate(
        "owner/channel",
        roster.channel_id(),
        "admin-fingerprint",
        "invite-2",
        EXPIRES,
    )
    .unwrap();
    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::CreateInvite {
            invite_id: other.invite_id.clone(),
            expires_at: other.expires_at.clone(),
        },
    );
    roster.apply(&entry).unwrap();

    // Built for invite-1, presented against invite-2.
    let ciphertext = request(&roster, &invite, &joiner);
    let error = review_join(&roster, &other, &admin.identity, &ciphertext, NOW).unwrap_err();

    assert!(matches!(error, CoreError::MalformedJoinRequest { .. }));
}

#[test]
fn a_forged_proof_is_refused() {
    // Someone who saw the invite ID in the control log but never held the
    // secret can produce everything else in the request.
    let (admin, joiner, roster, invite) = channel();

    let mut forged = invite.clone();
    forged.secret = canonical::encode_base64url(&[7u8; 32]);
    let ciphertext = request(&roster, &forged, &joiner);

    let error = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap_err();
    assert!(matches!(
        error,
        CoreError::Crypto(hrc_crypto::CryptoError::InviteProofInvalid)
    ));
}

#[test]
fn swapping_the_certificate_after_the_proof_is_refused() {
    // The proof commits to the certificate digest, so replacing the device
    // — and with it the encryption recipient a channel would address —
    // invalidates the proof rather than silently enrolling other hardware.
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let plaintext = admin.identity.decrypt(&ciphertext).unwrap();
    let mut signed: SignedObject<hrc_protocol::join::JoinRequestPayload> =
        canonical::from_json_str(std::str::from_utf8(&plaintext).unwrap()).unwrap();

    let attacker_identity = DeviceIdentity::generate();
    let attacker_device = SigningKey::from_seed(&[42; 32]);
    signed.payload.device_certificate = certify(
        &joiner.principal_key,
        &joiner.id,
        &attacker_device,
        &attacker_identity,
        42,
    );
    signed.signer.device_id = signed.payload.device_certificate.payload.device_id.clone();
    signed.signature = joiner
        .principal_key
        .sign_payload(domain::JOIN, &signed.payload)
        .unwrap();

    let resealed = reseal(&roster, &signed);
    let error = review_join(&roster, &invite, &admin.identity, &resealed, NOW).unwrap_err();

    assert!(matches!(
        error,
        CoreError::Crypto(hrc_crypto::CryptoError::InviteProofInvalid)
    ));
}

#[test]
fn a_certificate_signed_by_someone_else_is_refused() {
    // The claimed principal key must be the one that vouched for the device,
    // or a joiner could enroll a device they do not control.
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let plaintext = admin.identity.decrypt(&ciphertext).unwrap();
    let mut signed: SignedObject<hrc_protocol::join::JoinRequestPayload> =
        canonical::from_json_str(std::str::from_utf8(&plaintext).unwrap()).unwrap();

    let stranger = SigningKey::from_seed(&[77; 32]);
    signed.payload.device_certificate = certify(
        &stranger,
        &joiner.id,
        &joiner.device_key,
        &joiner.identity,
        3,
    );
    signed.signer.device_id = signed.payload.device_certificate.payload.device_id.clone();
    signed.signature = joiner
        .principal_key
        .sign_payload(domain::JOIN, &signed.payload)
        .unwrap();

    let resealed = reseal(&roster, &signed);
    let error = review_join(&roster, &invite, &admin.identity, &resealed, NOW).unwrap_err();

    assert!(matches!(error, CoreError::Crypto(_)), "{error}");
}

#[test]
fn a_tampered_request_signature_is_refused() {
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let plaintext = admin.identity.decrypt(&ciphertext).unwrap();
    let mut signed: SignedObject<hrc_protocol::join::JoinRequestPayload> =
        canonical::from_json_str(std::str::from_utf8(&plaintext).unwrap()).unwrap();

    signed.payload.created_at = "2026-09-13T03:00:00Z".into();

    let resealed = reseal(&roster, &signed);
    let error = review_join(&roster, &invite, &admin.identity, &resealed, NOW).unwrap_err();
    assert!(matches!(error, CoreError::Crypto(_)), "{error}");
}

#[test]
fn a_request_for_another_channel_is_refused() {
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);

    let plaintext = admin.identity.decrypt(&ciphertext).unwrap();
    let mut signed: SignedObject<hrc_protocol::join::JoinRequestPayload> =
        canonical::from_json_str(std::str::from_utf8(&plaintext).unwrap()).unwrap();

    signed.payload.channel_id = canonical::sha256_hex(b"a different channel");
    signed.signature = joiner
        .principal_key
        .sign_payload(domain::JOIN, &signed.payload)
        .unwrap();

    let resealed = reseal(&roster, &signed);
    let error = review_join(&roster, &invite, &admin.identity, &resealed, NOW).unwrap_err();
    assert!(matches!(error, CoreError::WrongChannel { .. }));
}

#[test]
fn an_existing_member_cannot_enroll_again() {
    let (admin, joiner, mut roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);
    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();
    let entry = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();
    roster.apply(&entry).unwrap();

    // A second invite, so what fails is the duplicate membership rather than
    // the spent authorization.
    let second = Invite::generate(
        "owner/channel",
        roster.channel_id(),
        "admin-fingerprint",
        "invite-2",
        EXPIRES,
    )
    .unwrap();
    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::CreateInvite {
            invite_id: second.invite_id.clone(),
            expires_at: second.expires_at.clone(),
        },
    );
    roster.apply(&entry).unwrap();

    let ciphertext = request(&roster, &second, &joiner);
    let error = review_join(&roster, &second, &admin.identity, &ciphertext, NOW).unwrap_err();
    assert!(matches!(error, CoreError::AlreadyEnrolled { .. }));
}

#[test]
fn an_ordinary_member_cannot_read_a_pending_join() {
    // Enrollments are addressed to administrators. A member who could
    // decrypt them would see every applicant's identity before the
    // administrator decided anything.
    let (admin, joiner, mut roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);
    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();
    let entry = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();
    roster.apply(&entry).unwrap();

    let second = Invite::generate(
        "owner/channel",
        roster.channel_id(),
        "admin-fingerprint",
        "invite-2",
        EXPIRES,
    )
    .unwrap();
    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::CreateInvite {
            invite_id: second.invite_id.clone(),
            expires_at: second.expires_at.clone(),
        },
    );
    roster.apply(&entry).unwrap();

    let third = TestPrincipal::new("third", 5);
    let ciphertext = request(&roster, &second, &third);

    // `joiner` is now an ordinary member of the channel.
    let error = review_join(&roster, &second, &joiner.identity, &ciphertext, NOW).unwrap_err();
    assert!(matches!(error, CoreError::Crypto(_)), "{error}");
}

#[test]
fn a_non_administrator_cannot_produce_an_admission() {
    let (admin, joiner, mut roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);
    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();
    let entry = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();
    roster.apply(&entry).unwrap();

    let second = Invite::generate(
        "owner/channel",
        roster.channel_id(),
        "admin-fingerprint",
        "invite-2",
        EXPIRES,
    )
    .unwrap();
    let entry = control_entry(
        &roster,
        &admin,
        ControlOperation::CreateInvite {
            invite_id: second.invite_id.clone(),
            expires_at: second.expires_at.clone(),
        },
    );
    roster.apply(&entry).unwrap();

    let third = TestPrincipal::new("third", 5);
    let ciphertext = request(&roster, &second, &third);
    let pending = review_join(&roster, &second, &admin.identity, &ciphertext, NOW).unwrap();

    // The now-ordinary member tries to admit the third principal.
    let error = admit(&roster, &joiner.device_key, joiner.signer(), &pending, NOW).unwrap_err();
    assert!(matches!(error, CoreError::UnauthorizedSigner { .. }));
}

#[test]
fn the_admission_entry_carries_the_joiner_and_chains_correctly() {
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);
    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();

    let entry = admit(&roster, &admin.device_key, admin.signer(), &pending, NOW).unwrap();

    assert_eq!(entry.payload.sequence, roster.sequence() + 1);
    assert_eq!(entry.payload.previous_hash, roster.head_hash());
    assert_eq!(entry.payload.epoch, roster.epoch() + 1);

    let ControlOperation::AddMember { invite_id, member } = &entry.payload.operation else {
        panic!("expected an add_member operation");
    };
    assert_eq!(invite_id, "invite-1");
    assert_eq!(member.principal_id, "joiner");
    assert_eq!(
        member.principal_signing_key,
        joiner.principal_key.verifying_key().to_base64url()
    );
    assert_eq!(member.devices.len(), 1);
}

#[test]
fn a_pending_join_carries_no_invite_secret() {
    // A pending join is shown in an interface and may be logged. The secret
    // that authorized it must not travel with it.
    let (admin, joiner, roster, invite) = channel();
    let ciphertext = request(&roster, &invite, &joiner);
    let pending = review_join(&roster, &invite, &admin.identity, &ciphertext, NOW).unwrap();

    let rendered = format!("{pending:?}");
    assert!(
        !rendered.contains(&invite.secret),
        "the invite secret reached a pending join"
    );
}

/// Re-encrypts a tampered request to the administrator, as an attacker with
/// the published recipient list could.
fn reseal(
    roster: &Roster,
    signed: &SignedObject<hrc_protocol::join::JoinRequestPayload>,
) -> Vec<u8> {
    let plaintext = canonical::to_canonical_bytes(signed).unwrap();
    let recipients = administrator_recipients(roster).unwrap();
    hrc_crypto::encrypt_to(&recipients, &plaintext).unwrap()
}
