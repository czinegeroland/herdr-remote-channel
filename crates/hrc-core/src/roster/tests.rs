//! Roster evaluation tests.
//!
//! These build real signed chains with real keys rather than stubs, because
//! the properties under test — authority, revocation ordering, chain
//! integrity — are exactly the ones a stub would fake.

use super::*;

use hrc_crypto::SigningKey;
use hrc_protocol::control::TransportLocator;
use hrc_protocol::identity::{DeviceCertificatePayload, DeviceDescriptor};
use hrc_protocol::signed::Signer;

/// A principal and one device, with the keys needed to sign as either.
struct TestPrincipal {
    id: String,
    principal_key: SigningKey,
    device_key: SigningKey,
    certificate: DeviceCertificate,
}

impl TestPrincipal {
    /// Creates a principal whose keys are derived from `seed`, so failures
    /// are reproducible.
    fn new(id: &str, seed: u8) -> Self {
        let principal_key = SigningKey::from_seed(&[seed; 32]);
        let device_key = SigningKey::from_seed(&[seed.wrapping_add(100); 32]);

        let descriptor = DeviceDescriptor {
            version: hrc_protocol::PROTOCOL_VERSION,
            principal_id: id.to_owned(),
            encryption_recipient: format!("age1{id}"),
            signing_key: device_key.verifying_key().to_base64url(),
            created_at: "2026-09-13T00:00:00Z".into(),
            expires_at: None,
            nonce: hrc_protocol::canonical::encode_base64url(&[seed; 16]),
        };

        let payload = DeviceCertificatePayload::new(descriptor).unwrap();
        let certificate = principal_key
            .sign_object(
                domain::DEVICE_CERTIFICATE,
                Signer {
                    principal_id: id.to_owned(),
                    device_id: payload.device_id.clone(),
                },
                payload,
            )
            .unwrap();

        Self {
            id: id.to_owned(),
            principal_key,
            device_key,
            certificate,
        }
    }

    /// A second device for the same principal.
    fn extra_device(&self, seed: u8) -> (SigningKey, DeviceCertificate) {
        let device_key = SigningKey::from_seed(&[seed; 32]);
        let descriptor = DeviceDescriptor {
            version: hrc_protocol::PROTOCOL_VERSION,
            principal_id: self.id.clone(),
            encryption_recipient: format!("age1{}-{seed}", self.id),
            signing_key: device_key.verifying_key().to_base64url(),
            created_at: "2026-09-13T01:00:00Z".into(),
            expires_at: None,
            nonce: hrc_protocol::canonical::encode_base64url(&[seed; 16]),
        };

        let payload = DeviceCertificatePayload::new(descriptor).unwrap();
        let certificate = self
            .principal_key
            .sign_object(
                domain::DEVICE_CERTIFICATE,
                Signer {
                    principal_id: self.id.clone(),
                    device_id: payload.device_id.clone(),
                },
                payload,
            )
            .unwrap();

        (device_key, certificate)
    }

    fn device_id(&self) -> String {
        self.certificate.payload.device_id.clone()
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
            device_id: self.device_id(),
        }
    }
}

/// Builds a signed genesis object for `admin`.
fn genesis_for(admin: &TestPrincipal) -> SignedObject<GenesisPayload> {
    let payload = GenesisPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        created_at: "2026-09-13T00:00:00Z".into(),
        initial_admin: admin.material(),
        transport: TransportLocator {
            kind: "git".into(),
            locator: "owner/channel".into(),
        },
        policy: serde_json::json!({}),
    };

    admin
        .device_key
        .sign_object(domain::GENESIS, admin.signer(), payload)
        .unwrap()
}

/// Signs the next control entry for `roster` with the given device key.
fn entry(
    roster: &Roster,
    signing_key: &SigningKey,
    signer: Signer,
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
        created_at: "2026-09-13T02:00:00Z".into(),
        operation,
    };

    signing_key
        .sign_object(domain::CONTROL, signer, payload)
        .unwrap()
}

/// The common case: an admin roster with one member added.
fn admin_and_member() -> (TestPrincipal, TestPrincipal, Roster) {
    let admin = TestPrincipal::new("admin", 1);
    let member = TestPrincipal::new("member", 2);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let add = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddMember {
            member: member.material(),
        },
    );
    roster.apply(&add).unwrap();

    (admin, member, roster)
}

#[test]
fn genesis_establishes_the_administrator_and_epoch_zero() {
    let admin = TestPrincipal::new("admin", 1);
    let roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    assert_eq!(roster.epoch(), 0);
    assert_eq!(roster.sequence(), 0);
    assert_eq!(roster.active_devices().count(), 1);

    let member = roster.members().next().unwrap();
    assert!(member.is_administrator);
    assert!(member.is_active);
    assert_eq!(member.principal_id, "admin");
}

#[test]
fn a_forged_genesis_signature_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let mut genesis = genesis_for(&admin);
    genesis.signature = SigningKey::from_seed(&[9; 32])
        .sign_payload(domain::GENESIS, &genesis.payload)
        .unwrap();

    assert!(matches!(
        Roster::from_genesis(&genesis).unwrap_err(),
        CoreError::Crypto(_)
    ));
}

#[test]
fn genesis_signed_by_a_device_it_does_not_certify_is_rejected() {
    // Otherwise a signature from an unrelated device would establish a
    // channel whose administrator the roster cannot actually identify.
    let admin = TestPrincipal::new("admin", 1);
    let stranger = TestPrincipal::new("admin", 3);

    let payload = GenesisPayload {
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
        .sign_object(domain::GENESIS, stranger.signer(), payload)
        .unwrap();

    assert!(matches!(
        Roster::from_genesis(&genesis).unwrap_err(),
        CoreError::UnknownDevice { .. }
    ));
}

#[test]
fn genesis_with_a_certificate_signed_by_another_principal_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let impostor = TestPrincipal::new("admin", 4);

    let mut material = admin.material();
    material.devices = vec![impostor.certificate.clone()];

    let payload = GenesisPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        created_at: "2026-09-13T00:00:00Z".into(),
        initial_admin: material,
        transport: TransportLocator {
            kind: "git".into(),
            locator: "owner/channel".into(),
        },
        policy: serde_json::json!({}),
    };
    let genesis = admin
        .device_key
        .sign_object(domain::GENESIS, admin.signer(), payload)
        .unwrap();

    assert!(matches!(
        Roster::from_genesis(&genesis).unwrap_err(),
        CoreError::Crypto(_)
    ));
}

#[test]
fn adding_a_member_advances_the_epoch() {
    let (_admin, member, roster) = admin_and_member();

    assert_eq!(roster.epoch(), 1);
    assert_eq!(roster.sequence(), 1);
    assert_eq!(roster.active_devices().count(), 2);
    assert!(roster.was_authorized_in(&member.device_id(), 1));
}

#[test]
fn a_member_added_in_epoch_one_was_not_authorized_in_epoch_zero() {
    // Message validation depends on this: a device cannot be credited with
    // authority for epochs that predate its admission.
    let (_admin, member, roster) = admin_and_member();
    assert!(!roster.was_authorized_in(&member.device_id(), 0));
}

#[test]
fn invites_and_policy_do_not_advance_the_epoch() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    for operation in [
        ControlOperation::CreateInvite {
            invite_id: "invite-1".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
        ControlOperation::UpdatePolicy {
            policy: serde_json::json!({ "readReceipts": true }),
        },
    ] {
        let signed = entry(&roster, &admin.device_key, admin.signer(), operation);
        roster.apply(&signed).unwrap();
    }

    assert_eq!(roster.epoch(), 0, "no membership change occurred");
    assert_eq!(roster.sequence(), 2);
}

#[test]
fn a_revoked_device_loses_authority_from_the_revoking_epoch() {
    let (admin, member, mut roster) = admin_and_member();
    let member_device = member.device_id();

    let revoke = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RevokeDevice {
            device_id: member_device.clone(),
        },
    );
    roster.apply(&revoke).unwrap();

    assert_eq!(roster.epoch(), 2);
    // Authorized while it was active, and not afterwards.
    assert!(roster.was_authorized_in(&member_device, 1));
    assert!(!roster.was_authorized_in(&member_device, 2));
    assert_eq!(roster.active_devices().count(), 1);

    // The device is retained, not deleted: old messages still need to
    // resolve their signer.
    assert!(roster.device(&member_device).is_some());
}

#[test]
fn a_revoked_device_cannot_sign_a_later_control_entry() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let (second_key, second_certificate) = admin.extra_device(60);
    let second_device_id = second_certificate.payload.device_id.clone();

    let add = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddDevice {
            certificate: second_certificate,
        },
    );
    roster.apply(&add).unwrap();

    let revoke = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RevokeDevice {
            device_id: second_device_id.clone(),
        },
    );
    roster.apply(&revoke).unwrap();

    let after_revocation = entry(
        &roster,
        &second_key,
        Signer {
            principal_id: admin.id.clone(),
            device_id: second_device_id,
        },
        ControlOperation::CreateInvite {
            invite_id: "invite-2".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
    );

    assert!(matches!(
        roster.apply(&after_revocation).unwrap_err(),
        CoreError::RevokedSigner { .. }
    ));
}

#[test]
fn an_administrator_can_revoke_their_own_device() {
    // Authority is evaluated against the state before the entry, which is
    // what makes revoking a compromised administrator device possible.
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let (_key, certificate) = admin.extra_device(61);
    let spare = certificate.payload.device_id.clone();
    let add = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddDevice { certificate },
    );
    roster.apply(&add).unwrap();

    let revoke_self = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RevokeDevice {
            device_id: admin.device_id(),
        },
    );
    roster.apply(&revoke_self).unwrap();

    assert!(!roster.was_authorized_in(&admin.device_id(), roster.epoch()));
    assert!(roster.was_authorized_in(&spare, roster.epoch()));
}

#[test]
fn removing_a_member_revokes_all_of_its_devices() {
    let (admin, member, mut roster) = admin_and_member();

    let remove = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RemoveMember {
            principal_id: member.id.clone(),
        },
    );
    roster.apply(&remove).unwrap();

    assert!(!roster.was_authorized_in(&member.device_id(), roster.epoch()));
    assert_eq!(roster.active_devices().count(), 1);
    assert!(
        roster
            .members()
            .any(|m| m.principal_id == member.id && !m.is_active)
    );
}

#[test]
fn the_sole_administrator_cannot_be_removed() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let remove = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RemoveMember {
            principal_id: admin.id.clone(),
        },
    );

    assert!(matches!(
        roster.apply(&remove).unwrap_err(),
        CoreError::CannotRemoveAdministrator
    ));
}

#[test]
fn a_non_administrator_cannot_sign_control_entries() {
    let (_admin, member, mut roster) = admin_and_member();

    let forged = entry(
        &roster,
        &member.device_key,
        member.signer(),
        ControlOperation::RevokeDevice {
            device_id: member.device_id(),
        },
    );

    assert!(matches!(
        roster.apply(&forged).unwrap_err(),
        CoreError::UnauthorizedSigner { .. }
    ));
}

#[test]
fn an_entry_signed_by_an_unknown_device_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let outsider = TestPrincipal::new("outsider", 7);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let forged = entry(
        &roster,
        &outsider.device_key,
        outsider.signer(),
        ControlOperation::UpdatePolicy {
            policy: serde_json::json!({}),
        },
    );

    assert!(matches!(
        roster.apply(&forged).unwrap_err(),
        CoreError::UnknownDevice { .. }
    ));
}

#[test]
fn a_tampered_entry_fails_signature_verification() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let mut signed = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::CreateInvite {
            invite_id: "invite-1".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
    );
    signed.payload.operation = ControlOperation::CreateInvite {
        invite_id: "invite-attacker".into(),
        expires_at: "2027-09-14T00:00:00Z".into(),
    };

    assert!(matches!(
        roster.apply(&signed).unwrap_err(),
        CoreError::Crypto(_)
    ));
}

#[test]
fn a_broken_predecessor_hash_is_rejected() {
    // This is the history-rewrite signal: the entry claims a parent that is
    // not the head this client observed.
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let mut signed = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::UpdatePolicy {
            policy: serde_json::json!({}),
        },
    );
    signed.payload.previous_hash = hrc_protocol::canonical::sha256_hex(b"a different history");
    signed.signature = admin
        .device_key
        .sign_payload(domain::CONTROL, &signed.payload)
        .unwrap();

    assert!(matches!(
        roster.apply(&signed).unwrap_err(),
        CoreError::BrokenChain { .. }
    ));
}

#[test]
fn a_competing_successor_at_the_same_sequence_is_rejected() {
    // Two valid entries claiming the same position is the observable fork
    // case from PRD section 17.5.2.
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let first = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::CreateInvite {
            invite_id: "invite-1".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
    );
    let competing = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::CreateInvite {
            invite_id: "invite-2".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
    );

    roster.apply(&first).unwrap();
    assert!(matches!(
        roster.apply(&competing).unwrap_err(),
        CoreError::SequenceOutOfOrder { .. }
    ));
}

#[test]
fn a_skipped_sequence_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let mut signed = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::UpdatePolicy {
            policy: serde_json::json!({}),
        },
    );
    signed.payload.sequence = 5;
    signed.signature = admin
        .device_key
        .sign_payload(domain::CONTROL, &signed.payload)
        .unwrap();

    assert!(matches!(
        roster.apply(&signed).unwrap_err(),
        CoreError::SequenceOutOfOrder {
            expected: 1,
            found: 5
        }
    ));
}

#[test]
fn an_entry_declaring_the_wrong_epoch_is_rejected() {
    // A membership change that does not advance the epoch would leave
    // revoked devices addressable under the unchanged epoch.
    let admin = TestPrincipal::new("admin", 1);
    let member = TestPrincipal::new("member", 2);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let mut signed = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddMember {
            member: member.material(),
        },
    );
    signed.payload.epoch = 0;
    signed.signature = admin
        .device_key
        .sign_payload(domain::CONTROL, &signed.payload)
        .unwrap();

    assert!(matches!(
        roster.apply(&signed).unwrap_err(),
        CoreError::EpochOutOfOrder {
            expected: 1,
            found: 0
        }
    ));
}

#[test]
fn an_entry_for_another_channel_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let mut signed = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::UpdatePolicy {
            policy: serde_json::json!({}),
        },
    );
    signed.payload.channel_id = hrc_protocol::canonical::sha256_hex(b"another channel");
    signed.signature = admin
        .device_key
        .sign_payload(domain::CONTROL, &signed.payload)
        .unwrap();

    assert!(matches!(
        roster.apply(&signed).unwrap_err(),
        CoreError::WrongChannel { .. }
    ));
}

#[test]
fn a_rejected_entry_leaves_the_roster_unchanged() {
    let (_admin, member, mut roster) = admin_and_member();
    let epoch = roster.epoch();
    let sequence = roster.sequence();
    let head = roster.head_hash().to_owned();
    let active = roster.active_devices().count();

    let forged = entry(
        &roster,
        &member.device_key,
        member.signer(),
        ControlOperation::RemoveMember {
            principal_id: member.id.clone(),
        },
    );
    assert!(roster.apply(&forged).is_err());

    assert_eq!(roster.epoch(), epoch);
    assert_eq!(roster.sequence(), sequence);
    assert_eq!(roster.head_hash(), head);
    assert_eq!(roster.active_devices().count(), active);
}

#[test]
fn rotating_a_device_key_revokes_the_old_device_and_admits_the_new_one() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let (_new_key, replacement) = admin.extra_device(62);
    let replacement_id = replacement.payload.device_id.clone();

    let rotate = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RotateDeviceKey {
            previous_device_id: admin.device_id(),
            certificate: replacement,
        },
    );
    roster.apply(&rotate).unwrap();

    assert!(!roster.was_authorized_in(&admin.device_id(), roster.epoch()));
    assert!(roster.was_authorized_in(&replacement_id, roster.epoch()));
    assert_eq!(roster.active_devices().count(), 1);
}

#[test]
fn a_device_certificate_for_a_non_member_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let outsider = TestPrincipal::new("outsider", 8);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let add = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddDevice {
            certificate: outsider.certificate.clone(),
        },
    );

    assert!(matches!(
        roster.apply(&add).unwrap_err(),
        CoreError::UnknownMember { .. }
    ));
}

#[test]
fn a_tampered_device_descriptor_is_rejected() {
    let admin = TestPrincipal::new("admin", 1);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let (_key, mut certificate) = admin.extra_device(63);
    certificate.payload.descriptor.encryption_recipient = "age1attacker".into();

    let add = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddDevice { certificate },
    );

    // The device ID no longer describes its descriptor, which is checked
    // before the principal signature.
    assert!(matches!(
        roster.apply(&add).unwrap_err(),
        CoreError::Protocol(hrc_protocol::ProtocolError::DerivedMismatch { .. })
    ));
}

#[test]
fn the_same_member_cannot_be_added_twice() {
    let (admin, member, mut roster) = admin_and_member();

    let again = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddMember {
            member: member.material(),
        },
    );

    assert!(matches!(
        roster.apply(&again).unwrap_err(),
        CoreError::DuplicateMember { .. }
    ));
}

#[test]
fn a_device_cannot_be_revoked_twice() {
    let (admin, member, mut roster) = admin_and_member();

    let revoke = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RevokeDevice {
            device_id: member.device_id(),
        },
    );
    roster.apply(&revoke).unwrap();

    let again = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RevokeDevice {
            device_id: member.device_id(),
        },
    );

    assert!(matches!(
        roster.apply(&again).unwrap_err(),
        CoreError::AlreadyRevoked { .. }
    ));
}

#[test]
fn rotating_a_principal_key_changes_which_certificates_verify() {
    let (admin, member, mut roster) = admin_and_member();
    let replacement_principal_key = SigningKey::from_seed(&[42; 32]);

    let rotate = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::RotatePrincipalKey {
            principal_id: member.id.clone(),
            principal_signing_key: replacement_principal_key.verifying_key().to_base64url(),
        },
    );
    roster.apply(&rotate).unwrap();

    // A device certificate signed by the retired principal key no longer
    // verifies against the rotated one.
    let (_key, stale) = member.extra_device(64);
    let add = entry(
        &roster,
        &admin.device_key,
        admin.signer(),
        ControlOperation::AddDevice { certificate: stale },
    );

    assert!(matches!(
        roster.apply(&add).unwrap_err(),
        CoreError::Crypto(_)
    ));
}

#[test]
fn a_full_chain_replays_to_the_expected_state() {
    // End-to-end: create, invite, add a member, add a device, revoke it.
    let admin = TestPrincipal::new("admin", 1);
    let member = TestPrincipal::new("member", 2);
    let mut roster = Roster::from_genesis(&genesis_for(&admin)).unwrap();

    let (_key, member_second) = member.extra_device(65);
    let member_second_id = member_second.payload.device_id.clone();

    let operations = vec![
        ControlOperation::CreateInvite {
            invite_id: "invite-1".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
        ControlOperation::AddMember {
            member: member.material(),
        },
        ControlOperation::AddDevice {
            certificate: member_second,
        },
        ControlOperation::RevokeDevice {
            device_id: member.device_id(),
        },
    ];

    for operation in operations {
        let signed = entry(&roster, &admin.device_key, admin.signer(), operation);
        roster.apply(&signed).unwrap();
    }

    assert_eq!(roster.sequence(), 4);
    assert_eq!(
        roster.epoch(),
        3,
        "three of four operations changed membership"
    );
    assert_eq!(roster.active_devices().count(), 2);
    assert!(roster.was_authorized_in(&member.device_id(), 1));
    assert!(!roster.was_authorized_in(&member.device_id(), 3));
    assert!(roster.was_authorized_in(&member_second_id, 3));
}
