//! Enrollment tests.

use super::*;

const CHANNEL: &str = "9f2c";
const INVITE: &str = "invite-1";
const NOW: &str = "2026-09-13T00:00:00Z";

fn invite() -> Invite {
    Invite::generate(
        "owner/channel",
        CHANNEL,
        "admin-fingerprint",
        INVITE,
        "2026-09-14T00:00:00Z",
    )
    .unwrap()
}

#[test]
fn a_generated_invite_meets_the_required_properties() {
    // PRD section 15.1.
    let invite = invite();
    invite.validate().unwrap();

    let secret = canonical::decode_base64url("secret", &invite.secret).unwrap();
    assert!(
        secret.len() * 8 >= 128,
        "an invite secret must carry at least 128 bits"
    );
    assert_eq!(invite.version, hrc_protocol::PROTOCOL_VERSION);
    assert!(!invite.expires_at.is_empty(), "an invite must expire");
}

#[test]
fn two_invites_never_share_a_secret() {
    assert_ne!(invite().secret, invite().secret);
}

#[test]
fn an_invite_code_round_trips() {
    let original = invite();
    let code = original.to_code().unwrap();

    assert!(code.starts_with("hrc1-"));
    assert_eq!(Invite::from_code(&code).unwrap(), original);
}

#[test]
fn a_malformed_code_is_rejected() {
    for code in [
        "",
        "hrc1",
        "hrc1-",
        "hrc2-abc",
        "hrc1-not base64url!",
        "hrc1-YWJj",
    ] {
        assert!(
            matches!(Invite::from_code(code), Err(CryptoError::MalformedInvite)),
            "{code:?} should be rejected"
        );
    }
}

#[test]
fn an_invite_with_a_weak_secret_is_rejected() {
    // Accepting a short secret would make the proof guessable, which is the
    // whole protection enrollment rests on.
    let mut weak = invite();
    weak.secret = canonical::encode_base64url(&[0u8; 8]);

    assert!(matches!(
        weak.validate(),
        Err(CryptoError::WeakInviteSecret {
            bits: 64,
            minimum_bits: 128
        })
    ));

    // And it cannot be smuggled in through a code either.
    let code = format!(
        "hrc1-{}",
        canonical::encode_base64url(&canonical::to_canonical_bytes(&weak).unwrap())
    );
    assert!(matches!(
        Invite::from_code(&code),
        Err(CryptoError::WeakInviteSecret { .. })
    ));
}

#[test]
fn expiry_is_compared_against_the_supplied_time() {
    let invite = invite();

    assert!(!invite.is_expired_at(NOW));
    assert!(invite.is_expired_at("2026-09-14T00:00:00Z"));
    assert!(invite.is_expired_at("2026-09-15T00:00:00Z"));
}

#[test]
fn a_proof_verifies_for_the_joiner_it_was_made_for() {
    let invite = invite();
    let proof = invite_proof(&invite, "joiner-key", "cert-hash").unwrap();

    verify_invite_proof(&invite, "joiner-key", "cert-hash", &proof).unwrap();
}

#[test]
fn a_proof_does_not_transfer_to_another_joiner() {
    // The proof commits to the joiner's key and certificate, so an
    // intercepted proof cannot be replayed to enroll a different identity.
    let invite = invite();
    let proof = invite_proof(&invite, "joiner-key", "cert-hash").unwrap();

    assert!(matches!(
        verify_invite_proof(&invite, "attacker-key", "cert-hash", &proof),
        Err(CryptoError::InviteProofInvalid)
    ));
    assert!(matches!(
        verify_invite_proof(&invite, "joiner-key", "other-cert", &proof),
        Err(CryptoError::InviteProofInvalid)
    ));
}

#[test]
fn a_proof_does_not_transfer_to_another_invite() {
    let first = invite();
    let proof = invite_proof(&first, "joiner-key", "cert-hash").unwrap();

    let mut other_channel = first.clone();
    other_channel.channel_id = "another-channel".into();
    assert!(matches!(
        verify_invite_proof(&other_channel, "joiner-key", "cert-hash", &proof),
        Err(CryptoError::InviteProofInvalid)
    ));

    let mut other_id = first.clone();
    other_id.invite_id = "invite-2".into();
    assert!(matches!(
        verify_invite_proof(&other_id, "joiner-key", "cert-hash", &proof),
        Err(CryptoError::InviteProofInvalid)
    ));

    // A different secret entirely: the classic forgery attempt.
    let unrelated = invite();
    assert!(matches!(
        verify_invite_proof(&unrelated, "joiner-key", "cert-hash", &proof),
        Err(CryptoError::InviteProofInvalid)
    ));
}

#[test]
fn a_garbage_proof_is_rejected_without_panicking() {
    let invite = invite();

    for proof in ["", "not base64url!", "AAAA", &"A".repeat(200)] {
        assert!(
            matches!(
                verify_invite_proof(&invite, "joiner-key", "cert-hash", proof),
                Err(CryptoError::InviteProofInvalid)
            ),
            "{proof:?} should be rejected"
        );
    }
}

#[test]
fn the_proof_never_contains_the_secret() {
    let invite = invite();
    let proof = invite_proof(&invite, "joiner-key", "cert-hash").unwrap();

    assert_ne!(proof, invite.secret);
    assert!(!proof.contains(&invite.secret));
}

#[test]
fn the_wordlist_is_the_expected_size_and_order() {
    // The derivation assumes 7776 entries; a truncated or reordered list
    // would silently change every phrase this build produces.
    let words = wordlist();

    assert_eq!(words.len(), 7776);
    assert_eq!(words[0], "abacus");
    assert_eq!(words[7775], "zoom");
    assert!(
        words.iter().all(|word| !word.is_empty()),
        "no entry may be empty"
    );
}

#[test]
fn a_safety_phrase_is_six_words_from_the_named_list() {
    let phrase = safety_phrase(CHANNEL, INVITE, "admin-key", "joiner-key", "cert-hash").unwrap();

    assert_eq!(phrase.words.len(), 6);
    assert_eq!(phrase.wordlist, "eff-large-2016");

    let words = wordlist();
    assert!(phrase.words.iter().all(|word| words.contains(word)));
    assert_eq!(phrase.to_string().split(' ').count(), 6);
}

#[test]
fn the_same_inputs_always_give_the_same_phrase() {
    // Both sides derive independently; if this were not stable they could
    // never agree.
    let first = safety_phrase(CHANNEL, INVITE, "admin-key", "joiner-key", "cert-hash").unwrap();
    let second = safety_phrase(CHANNEL, INVITE, "admin-key", "joiner-key", "cert-hash").unwrap();

    assert_eq!(first, second);
}

#[test]
fn substituting_any_identity_changes_the_phrase() {
    // This is what the comparison detects. Each input is checked separately
    // so a field silently dropped from the derivation would fail here.
    let baseline = safety_phrase(CHANNEL, INVITE, "admin-key", "joiner-key", "cert-hash").unwrap();

    let variants = [
        safety_phrase(
            "other-channel",
            INVITE,
            "admin-key",
            "joiner-key",
            "cert-hash",
        ),
        safety_phrase(CHANNEL, "invite-2", "admin-key", "joiner-key", "cert-hash"),
        safety_phrase(CHANNEL, INVITE, "attacker-key", "joiner-key", "cert-hash"),
        safety_phrase(CHANNEL, INVITE, "admin-key", "attacker-key", "cert-hash"),
        safety_phrase(CHANNEL, INVITE, "admin-key", "joiner-key", "other-cert"),
    ];

    for variant in variants {
        assert_ne!(
            variant.unwrap(),
            baseline,
            "a substituted identity must change the phrase"
        );
    }
}

#[test]
fn the_phrase_derivation_matches_a_pinned_vector() {
    // Pins the whole chain: canonical encoding, domain separation, the bit
    // slice, the base-7776 conversion, and the wordlist order. Any change to
    // one of them breaks agreement between peers, so it should break here
    // first.
    let phrase = safety_phrase(
        "channel-id",
        "invite-id",
        "admin-public-key",
        "joiner-public-key",
        "certificate-hash",
    )
    .unwrap();

    assert_eq!(phrase.to_string(), PINNED_PHRASE);
}

/// Derived by an independent implementation of PRD section 18.0.3, not
/// recorded from this crate's own output. Indices [1646, 7093, 595, 215,
/// 976, 2222] into the vendored EFF list.
const PINNED_PHRASE: &str = "deflator unelected bobtail annually charm encircle";

#[test]
fn phrase_indices_cover_the_whole_wordlist_range() {
    // A conversion bug that clamped or masked indices would concentrate
    // phrases in one part of the list. Sample enough inputs to notice.
    let mut lowest = usize::MAX;
    let mut highest = 0usize;
    let words = wordlist();

    for index in 0..600 {
        let phrase = safety_phrase(
            &format!("channel-{index}"),
            INVITE,
            "admin-key",
            "joiner-key",
            "cert-hash",
        )
        .unwrap();

        for word in &phrase.words {
            let position = words
                .iter()
                .position(|candidate| candidate == word)
                .unwrap();
            lowest = lowest.min(position);
            highest = highest.max(position);
        }
    }

    assert!(lowest < 200, "no low indices appeared: lowest was {lowest}");
    assert!(
        highest > 7_500,
        "no high indices appeared: highest was {highest}"
    );
}

#[test]
fn the_first_digest_bits_drive_the_phrase() {
    // Two digests differing only in the low bits must still agree, and two
    // differing in the high bits must not. This is what makes the 77-bit
    // slice the actual source of the words.
    let mut digest = [0u8; 32];
    let base = phrase_from_digest(&digest);

    digest[31] = 0xff;
    assert_eq!(
        phrase_from_digest(&digest),
        base,
        "bits beyond the first 77 must not affect the phrase"
    );

    digest = [0u8; 32];
    digest[0] = 0x80;
    assert_ne!(phrase_from_digest(&digest), base);
}
