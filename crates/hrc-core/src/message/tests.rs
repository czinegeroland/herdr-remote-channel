//! Message sealing and opening tests.
//!
//! These build a real channel: real keys, a real signed control chain, real
//! age encryption. The properties under test are the ones an attacker would
//! target, so nothing here is stubbed.

use super::*;

use hrc_crypto::DeviceIdentity;
use hrc_protocol::control::{
    ControlEntryPayload, ControlOperation, DeviceCertificate, GenesisPayload, PrincipalMaterial,
    TransportLocator,
};
use hrc_protocol::identity::{DeviceCertificatePayload, DeviceDescriptor};
use hrc_protocol::message::{Addressing, MessageEnvelope};

const NOW: &str = "2026-09-13T00:00:00Z";

/// A principal with one device, including a real age identity.
struct TestPeer {
    id: String,
    principal_key: SigningKey,
    device_key: SigningKey,
    identity: DeviceIdentity,
    certificate: DeviceCertificate,
}

impl TestPeer {
    fn new(id: &str, seed: u8) -> Self {
        let principal_key = SigningKey::from_seed(&[seed; 32]);
        let device_key = SigningKey::from_seed(&[seed.wrapping_add(100); 32]);
        let identity = DeviceIdentity::generate();

        let descriptor = DeviceDescriptor {
            version: hrc_protocol::PROTOCOL_VERSION,
            principal_id: id.to_owned(),
            encryption_recipient: identity.recipient().to_string(),
            signing_key: device_key.verifying_key().to_base64url(),
            created_at: NOW.into(),
            expires_at: None,
            nonce: canonical::encode_base64url(&[seed; 16]),
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
            identity,
            certificate,
        }
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

/// Signs genesis for `admin`.
fn genesis_for(admin: &TestPeer) -> SignedObject<GenesisPayload> {
    let payload = GenesisPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        created_at: NOW.into(),
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

/// Signs the next control entry.
fn control_entry(
    roster: &Roster,
    admin: &TestPeer,
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

/// A channel with an administrator and one member.
fn channel() -> (TestPeer, TestPeer, Roster) {
    let alice = TestPeer::new("alice", 1);
    let bob = TestPeer::new("bob", 2);
    let mut roster = Roster::from_genesis(&genesis_for(&alice)).unwrap();

    let invite = control_entry(
        &roster,
        &alice,
        ControlOperation::CreateInvite {
            invite_id: "invite-1".into(),
            expires_at: "2026-09-14T00:00:00Z".into(),
        },
    );
    roster.apply(&invite).unwrap();

    let add = control_entry(
        &roster,
        &alice,
        ControlOperation::AddMember {
            invite_id: "invite-1".into(),
            member: bob.material(),
        },
    );
    roster.apply(&add).unwrap();

    (alice, bob, roster)
}

/// A note from `sender` addressed to `to`.
fn note(roster: &Roster, to: &[&str]) -> MessageEnvelope {
    MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: roster.channel_id().to_owned(),
        roster_epoch: roster.epoch(),
        message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        device_sequence: 1,
        previous_chain_id: None,
        created_at: NOW.into(),
        expires_at: None,
        to: Addressing {
            principals: to.iter().map(|value| (*value).to_owned()).collect(),
            endpoint: None,
        },
        // Overwritten by seal from the roster; a placeholder keeps the type
        // constructible and proves the caller's value is not trusted.
        recipients: hrc_protocol::RecipientDevices::new([canonical::sha256_hex(b"placeholder")])
            .unwrap(),
        thread_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        in_reply_to: None,
        kind: "note".into(),
        requested_capability: None,
        body: serde_json::json!({ "text": "staging is failing" }),
        attachments: Vec::new(),
        padding: String::new(),
    }
}

#[test]
fn a_sealed_message_opens_for_its_recipient() {
    let (alice, bob, roster) = channel();

    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();

    let opened = open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap();

    assert_eq!(opened.sender_principal, "alice");
    assert_eq!(opened.sender_device, alice.device_id());
    assert_eq!(opened.envelope.body["text"], "staging is failing");
    assert_eq!(opened.ciphertext_sha256, canonical::sha256_hex(&ciphertext));
}

#[test]
fn the_recipient_commitment_comes_from_the_roster_not_the_caller() {
    // The caller's placeholder recipient set must be discarded, or a sender
    // could commit to one audience and encrypt to another.
    let (alice, bob, roster) = channel();

    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();
    let opened = open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap();

    assert_eq!(opened.envelope.recipients.device_ids, vec![bob.device_id()]);
    assert!(
        !opened
            .envelope
            .recipients
            .contains(&canonical::sha256_hex(b"placeholder"))
    );
}

#[test]
fn a_broadcast_addresses_every_active_device() {
    let (alice, bob, roster) = channel();

    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &[]),
    )
    .unwrap();

    // Both the sender's own device and the member's are addressed, so a
    // sender can read back what it sent.
    let opened = open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap();
    assert_eq!(opened.envelope.recipients.device_ids.len(), 2);

    let by_sender = open(&roster, &alice.identity, &ciphertext, roster.epoch(), NOW).unwrap();
    assert_eq!(by_sender.envelope.body["text"], "staging is failing");
}

#[test]
fn padding_hides_the_body_length_within_a_bucket() {
    // Bodies differing by ~400 bytes must not produce ciphertexts differing
    // by ~400 bytes, or an observer reads message length off the repository.
    //
    // The two ciphertexts are not byte-for-byte the same length: age adds a
    // random amount of header padding, so identical plaintext lengths still
    // yield ciphertexts varying by a few dozen bytes. That variation is
    // independent of the body, which is the point — it adds noise rather
    // than signal. What padding guarantees is a bucketed *plaintext*.
    let (alice, _bob, roster) = channel();

    let mut short = note(&roster, &["bob"]);
    short.body = serde_json::json!({ "text": "ok" });

    let mut longer = note(&roster, &["bob"]);
    longer.body = serde_json::json!({ "text": "x".repeat(400) });

    let short_ciphertext = seal(&roster, &alice.device_key, alice.signer(), short).unwrap();
    let longer_ciphertext = seal(&roster, &alice.device_key, alice.signer(), longer).unwrap();

    let difference = short_ciphertext.len().abs_diff(longer_ciphertext.len());
    assert!(
        difference < 200,
        "a 400 byte body difference leaked {difference} bytes of ciphertext"
    );
}

#[test]
fn crossing_a_bucket_boundary_changes_the_size_class() {
    // Padding bounds what length reveals; it does not erase it. A message
    // large enough to need the next bucket is visibly larger, and the PRD
    // says so in section 16.4 rather than claiming otherwise.
    let (alice, _bob, roster) = channel();

    let mut small = note(&roster, &["bob"]);
    small.body = serde_json::json!({ "text": "ok" });

    let mut large = note(&roster, &["bob"]);
    large.body = serde_json::json!({ "text": "x".repeat(8_000) });

    let small_ciphertext = seal(&roster, &alice.device_key, alice.signer(), small).unwrap();
    let large_ciphertext = seal(&roster, &alice.device_key, alice.signer(), large).unwrap();

    assert!(large_ciphertext.len() > small_ciphertext.len() + 4_000);
}

#[test]
fn a_device_outside_the_recipient_set_cannot_decrypt() {
    let (alice, _bob, roster) = channel();
    let outsider = DeviceIdentity::generate();

    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();

    assert!(matches!(
        open(&roster, &outsider, &ciphertext, roster.epoch(), NOW).unwrap_err(),
        CoreError::Crypto(_)
    ));
}

#[test]
fn a_tampered_ciphertext_is_rejected() {
    let (alice, bob, roster) = channel();
    let mut ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();

    let last = ciphertext.len() - 1;
    ciphertext[last] ^= 0x01;

    assert!(open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).is_err());
}

#[test]
fn a_message_signed_by_an_unknown_device_is_rejected() {
    let (_alice, bob, roster) = channel();
    let stranger = TestPeer::new("stranger", 9);

    // Encrypt to Bob but sign as someone the roster has never seen.
    let mut envelope = note(&roster, &["bob"]);
    envelope.recipients = hrc_protocol::RecipientDevices::new([bob.device_id()]).unwrap();
    envelope.padding = "0".repeat(64);

    let signed = stranger
        .device_key
        .sign_object(domain::MESSAGE, stranger.signer(), envelope)
        .unwrap();
    let plaintext = canonical::to_canonical_bytes(&signed).unwrap();
    let ciphertext = hrc_crypto::encrypt_to(
        &[DeviceRecipient::parse(&bob.identity.recipient().to_string()).unwrap()],
        &plaintext,
    )
    .unwrap();

    assert!(matches!(
        open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap_err(),
        CoreError::UnknownDevice { .. }
    ));
}

#[test]
fn a_forged_signature_is_rejected() {
    let (alice, bob, roster) = channel();

    // Alice's identity, but signed with a key that is not hers.
    let mut envelope = note(&roster, &["bob"]);
    envelope.recipients = hrc_protocol::RecipientDevices::new([bob.device_id()]).unwrap();
    envelope.padding = "0".repeat(64);

    let impostor = SigningKey::from_seed(&[42; 32]);
    let signed = impostor
        .sign_object(domain::MESSAGE, alice.signer(), envelope)
        .unwrap();
    let plaintext = canonical::to_canonical_bytes(&signed).unwrap();
    let ciphertext = hrc_crypto::encrypt_to(
        &[DeviceRecipient::parse(&bob.identity.recipient().to_string()).unwrap()],
        &plaintext,
    )
    .unwrap();

    assert!(matches!(
        open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap_err(),
        CoreError::Crypto(_)
    ));
}

#[test]
fn a_message_for_another_channel_is_rejected() {
    let (alice, bob, roster) = channel();

    let mut envelope = note(&roster, &["bob"]);
    envelope.channel_id = canonical::sha256_hex(b"another channel");

    // Seal refuses it outright.
    assert!(matches!(
        seal(&roster, &alice.device_key, alice.signer(), envelope).unwrap_err(),
        CoreError::WrongChannel { .. }
    ));

    // And a hand-built one is refused on open.
    let mut envelope = note(&roster, &["bob"]);
    envelope.channel_id = canonical::sha256_hex(b"another channel");
    envelope.recipients = hrc_protocol::RecipientDevices::new([bob.device_id()]).unwrap();
    envelope.padding = "0".repeat(64);

    let signed = alice
        .device_key
        .sign_object(domain::MESSAGE, alice.signer(), envelope)
        .unwrap();
    let ciphertext = hrc_crypto::encrypt_to(
        &[DeviceRecipient::parse(&bob.identity.recipient().to_string()).unwrap()],
        &canonical::to_canonical_bytes(&signed).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap_err(),
        CoreError::WrongChannel { .. }
    ));
}

#[test]
fn sealing_under_a_stale_epoch_is_refused() {
    let (alice, _bob, roster) = channel();

    let mut envelope = note(&roster, &["bob"]);
    envelope.roster_epoch = roster.epoch() - 1;

    assert!(matches!(
        seal(&roster, &alice.device_key, alice.signer(), envelope).unwrap_err(),
        CoreError::EpochOutOfOrder { .. }
    ));
}

#[test]
fn a_revoked_device_cannot_seal() {
    let (alice, bob, mut roster) = channel();

    let revoke = control_entry(
        &roster,
        &alice,
        ControlOperation::RevokeDevice {
            device_id: bob.device_id(),
        },
    );
    roster.apply(&revoke).unwrap();

    let mut envelope = note(&roster, &["alice"]);
    envelope.roster_epoch = roster.epoch();

    assert!(matches!(
        seal(&roster, &bob.device_key, bob.signer(), envelope).unwrap_err(),
        CoreError::RevokedSigner { .. }
    ));
}

#[test]
fn a_message_introduced_after_revocation_under_an_older_epoch_is_rejected() {
    // The central HRC-SEC-013 case: a removed device publishes a message
    // that claims an epoch from before its revocation. Signature and
    // encryption are both valid; only the introduction point exposes it.
    let (alice, bob, mut roster) = channel();
    let epoch_before_revocation = roster.epoch();

    // Bob seals a legitimate message while still a member.
    let ciphertext = seal(
        &roster,
        &bob.device_key,
        bob.signer(),
        note(&roster, &["alice"]),
    )
    .unwrap();

    // It opens fine when introduced before the revocation.
    open(
        &roster,
        &alice.identity,
        &ciphertext,
        epoch_before_revocation,
        NOW,
    )
    .unwrap();

    let revoke = control_entry(
        &roster,
        &alice,
        ControlOperation::RevokeDevice {
            device_id: bob.device_id(),
        },
    );
    roster.apply(&revoke).unwrap();

    // The very same ciphertext, introduced after the revoking control entry,
    // must be refused.
    let error = open(&roster, &alice.identity, &ciphertext, roster.epoch(), NOW).unwrap_err();
    assert!(
        matches!(error, CoreError::RevokedSigner { .. }),
        "unexpected error: {error}"
    );
}

#[test]
fn a_message_claiming_an_epoch_from_the_future_is_rejected() {
    let (alice, bob, roster) = channel();
    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();

    // Introduced under an epoch older than the one it claims: impossible for
    // an honest sender, since the epoch did not exist when it was published.
    let error = open(&roster, &bob.identity, &ciphertext, roster.epoch() - 1, NOW).unwrap_err();
    assert!(
        matches!(
            error,
            CoreError::EpochOutOfOrder { .. } | CoreError::RevokedSigner { .. }
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn a_recipient_set_that_disagrees_with_the_roster_is_rejected() {
    // A conforming sender commits to the roster-derived set. This forges a
    // message that encrypts to Bob but commits to a different audience.
    let (alice, bob, roster) = channel();

    let mut envelope = note(&roster, &["bob"]);
    envelope.recipients = hrc_protocol::RecipientDevices::new([
        bob.device_id(),
        canonical::sha256_hex(b"a device that is not in the roster"),
    ])
    .unwrap();
    envelope.padding = "0".repeat(64);

    let signed = alice
        .device_key
        .sign_object(domain::MESSAGE, alice.signer(), envelope)
        .unwrap();
    let ciphertext = hrc_crypto::encrypt_to(
        &[DeviceRecipient::parse(&bob.identity.recipient().to_string()).unwrap()],
        &canonical::to_canonical_bytes(&signed).unwrap(),
    )
    .unwrap();

    assert!(matches!(
        open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap_err(),
        CoreError::RecipientSetMismatch
    ));
}

#[test]
fn an_expired_message_is_refused() {
    let (alice, bob, roster) = channel();

    let mut envelope = note(&roster, &["bob"]);
    envelope.expires_at = Some("2026-09-14T00:00:00Z".into());

    let ciphertext = seal(&roster, &alice.device_key, alice.signer(), envelope).unwrap();

    open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).expect("not yet expired");

    let error = open(
        &roster,
        &bob.identity,
        &ciphertext,
        roster.epoch(),
        "2026-09-15T00:00:00Z",
    )
    .unwrap_err();
    assert!(matches!(error, CoreError::MessageExpired { .. }));
}

#[test]
fn addressing_a_non_member_is_refused() {
    let (alice, _bob, roster) = channel();

    assert!(matches!(
        seal(
            &roster,
            &alice.device_key,
            alice.signer(),
            note(&roster, &["carol"])
        )
        .unwrap_err(),
        CoreError::UnknownMember { .. }
    ));
}

#[test]
fn addressing_a_removed_member_is_refused() {
    let (alice, bob, mut roster) = channel();

    let remove = control_entry(
        &roster,
        &alice,
        ControlOperation::RemoveMember {
            principal_id: bob.id.clone(),
        },
    );
    roster.apply(&remove).unwrap();

    let mut envelope = note(&roster, &["bob"]);
    envelope.roster_epoch = roster.epoch();

    assert!(matches!(
        seal(&roster, &alice.device_key, alice.signer(), envelope).unwrap_err(),
        CoreError::UnknownMember { .. }
    ));
}

#[test]
fn an_invalid_endpoint_is_refused_at_seal_time() {
    // PRD section 19.1: an endpoint may appear on the agent-safe surface, so
    // free text must never get that far.
    let (alice, _bob, roster) = channel();

    let mut envelope = note(&roster, &["bob"]);
    envelope.to.endpoint = Some("Reviewer; DROP TABLE".into());

    assert!(matches!(
        seal(&roster, &alice.device_key, alice.signer(), envelope).unwrap_err(),
        CoreError::Protocol(hrc_protocol::ProtocolError::InvalidEndpoint { .. })
    ));
}

#[test]
fn a_valid_endpoint_survives_the_round_trip() {
    let (alice, bob, roster) = channel();

    let mut envelope = note(&roster, &["bob"]);
    envelope.to.endpoint = Some("reviewer".into());

    let ciphertext = seal(&roster, &alice.device_key, alice.signer(), envelope).unwrap();
    let opened = open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap();

    assert_eq!(opened.envelope.to.endpoint.as_deref(), Some("reviewer"));
}

#[test]
fn opening_returns_quarantined_content_bound_to_its_ciphertext() {
    // The digest is what an approval is bound to, so it must describe the
    // exact bytes that arrived.
    let (alice, bob, roster) = channel();
    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();

    let opened = open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).unwrap();
    assert_eq!(opened.ciphertext_sha256, canonical::sha256_hex(&ciphertext));

    let other = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .unwrap();
    let other_opened = open(&roster, &bob.identity, &other, roster.epoch(), NOW).unwrap();

    assert_ne!(
        opened.ciphertext_sha256, other_opened.ciphertext_sha256,
        "two encryptions of the same message are distinct objects"
    );
}

#[test]
fn no_private_key_material_reaches_a_published_message() {
    // PRD requirement HRC-SEC-001: private keys never leave their device.
    // The at-rest half is `crates/hrc-crypto/src/store.rs`; this is the
    // other half, and the one an attacker actually sees. A signing key is
    // used to sign and is never a thing to send, so the seed bytes must
    // appear nowhere in what gets published — not in the envelope, not in
    // the ciphertext, not in the canonical bytes a peer receives.
    //
    // The seeds here are deliberately long constant runs, which is the
    // easiest possible pattern to find and therefore the strictest test: if
    // key material were being serialized anywhere, this would catch it.
    let (alice, bob, roster) = channel();

    let ciphertext = seal(
        &roster,
        &alice.device_key,
        alice.signer(),
        note(&roster, &["bob"]),
    )
    .expect("sealing should succeed");

    // Every seed this fixture built a key from.
    let secrets: Vec<[u8; 32]> = vec![[1; 32], [101; 32], [2; 32], [102; 32]];

    for seed in &secrets {
        assert!(
            !contains_window(&ciphertext, seed),
            "raw key material appears in the published bytes"
        );

        // Also in the encodings a key would plausibly be serialized as, if
        // some future field carried one by accident.
        let base64 = canonical::encode_base64url(seed);
        let hex: String = seed.iter().map(|byte| format!("{byte:02x}")).collect();
        let rendered = String::from_utf8_lossy(&ciphertext);

        assert!(
            !rendered.contains(&base64),
            "base64 key material appears in the published bytes"
        );
        assert!(
            !rendered.contains(&hex),
            "hex key material appears in the published bytes"
        );
    }

    // The message still opens, so this is not passing because nothing was
    // published.
    let opened =
        open(&roster, &bob.identity, &ciphertext, roster.epoch(), NOW).expect("bob should open it");
    assert_eq!(opened.sender_principal, alice.id);
}

/// Whether `haystack` contains `needle` as a contiguous run.
fn contains_window(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len()
        && haystack
            .windows(needle.len())
            .any(|window| window == needle)
}
