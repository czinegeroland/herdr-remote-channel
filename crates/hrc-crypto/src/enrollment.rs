//! Invites, invite proofs, and the enrollment safety phrase.
//!
//! Enrollment is the moment a channel's membership can be attacked most
//! cheaply, so PRD section 15 constrains it tightly:
//!
//! - An invite authorizes a membership *request* and nothing else. It never
//!   derives a private key, never doubles as a group key, and never grants
//!   capabilities or approves its own join.
//! - The joiner proves possession of the invite secret without publishing
//!   it, using an HMAC over the values the request binds to.
//! - Both sides derive the same safety phrase from public material and
//!   compare it out of band. That is what detects a substituted key: an
//!   attacker who replaces either identity changes the phrase.
//!
//! The phrase is compared by humans. An agent cannot confirm it, because an
//! agent that could would be the thing an attacker compromises.

use hmac::{Hmac, KeyInit as _, Mac as _};
use hrc_protocol::canonical;
use serde::{Deserialize, Serialize};
use sha2::Sha256;

use crate::{CryptoError, Result};

/// The wordlist the safety phrase indexes into.
///
/// Vendored verbatim; see the file header for source and licence. Both
/// clients display this identifier with the phrase, so a future list can be
/// introduced without two peers silently comparing different vocabularies
/// (PRD section 18.0.3).
pub const WORDLIST_ID: &str = "eff-large-2016";

/// The vendored wordlist, comments stripped at load time.
const WORDLIST_SOURCE: &str = include_str!("../wordlists/eff_large_2016.txt");

/// How many words a safety phrase has.
const PHRASE_WORDS: usize = 6;

/// How many bits of the digest the phrase consumes (PRD section 18.0.3).
const PHRASE_BITS: u32 = 77;

/// Minimum invite secret length, in bytes (PRD section 15.1: 128 bits).
pub const MIN_INVITE_SECRET_BYTES: usize = 16;

/// The words of the vendored list, in file order.
fn wordlist() -> Vec<&'static str> {
    WORDLIST_SOURCE
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
}

/// Everything an invite code carries (PRD section 11.3).
///
/// The secret is part of the code and never part of the channel: publishing
/// it would let anyone who can read the repository enroll.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Invite {
    /// Protocol version.
    pub version: u32,
    /// Where the channel lives, for example `owner/name`.
    pub locator: String,
    /// The channel this invite admits to.
    #[serde(rename = "channelId")]
    pub channel_id: String,
    /// Fingerprint of the administrator's principal key.
    #[serde(rename = "adminFingerprint")]
    pub admin_fingerprint: String,
    /// Identifier of this invite, published in the control log.
    #[serde(rename = "inviteId")]
    pub invite_id: String,
    /// Unpadded base64url secret, at least 128 bits.
    pub secret: String,
    /// RFC 3339 UTC expiry.
    #[serde(rename = "expiresAt")]
    pub expires_at: String,
}

/// Prefix identifying an HRC invite code.
const INVITE_PREFIX: &str = "hrc1";

impl Invite {
    /// Generates an invite with a fresh secret.
    pub fn generate(
        locator: impl Into<String>,
        channel_id: impl Into<String>,
        admin_fingerprint: impl Into<String>,
        invite_id: impl Into<String>,
        expires_at: impl Into<String>,
    ) -> Result<Self> {
        let mut secret = [0u8; 32];
        getrandom::fill(&mut secret).map_err(CryptoError::Entropy)?;

        Ok(Self {
            version: hrc_protocol::PROTOCOL_VERSION,
            locator: locator.into(),
            channel_id: channel_id.into(),
            admin_fingerprint: admin_fingerprint.into(),
            invite_id: invite_id.into(),
            secret: canonical::encode_base64url(&secret),
            expires_at: expires_at.into(),
        })
    }

    /// Encodes the invite as a single transferable string.
    ///
    /// One opaque token is harder to transcribe partially than a set of
    /// fields, and it keeps the secret from being separated from the values
    /// it authorizes.
    pub fn to_code(&self) -> Result<String> {
        let json = canonical::to_canonical_bytes(self)?;
        Ok(format!(
            "{INVITE_PREFIX}-{}",
            canonical::encode_base64url(&json)
        ))
    }

    /// Parses an invite code.
    pub fn from_code(code: &str) -> Result<Self> {
        let body = code
            .trim()
            .strip_prefix(INVITE_PREFIX)
            .and_then(|rest| rest.strip_prefix('-'))
            .ok_or(CryptoError::MalformedInvite)?;

        let json = canonical::decode_base64url("invite", body)
            .map_err(|_| CryptoError::MalformedInvite)?;
        let text = std::str::from_utf8(&json).map_err(|_| CryptoError::MalformedInvite)?;

        let invite: Invite =
            canonical::from_json_str(text).map_err(|_| CryptoError::MalformedInvite)?;
        invite.validate()?;
        Ok(invite)
    }

    /// Checks the invariants PRD section 15.1 requires.
    pub fn validate(&self) -> Result<()> {
        if self.version != hrc_protocol::PROTOCOL_VERSION {
            return Err(CryptoError::MalformedInvite);
        }

        let secret = canonical::decode_base64url("secret", &self.secret)
            .map_err(|_| CryptoError::MalformedInvite)?;
        if secret.len() < MIN_INVITE_SECRET_BYTES {
            return Err(CryptoError::WeakInviteSecret {
                bits: secret.len() * 8,
                minimum_bits: MIN_INVITE_SECRET_BYTES * 8,
            });
        }

        if self.channel_id.is_empty() || self.invite_id.is_empty() || self.expires_at.is_empty() {
            return Err(CryptoError::MalformedInvite);
        }

        Ok(())
    }

    /// Whether this invite has expired at `now`.
    pub fn is_expired_at(&self, now: &str) -> bool {
        self.expires_at.as_str() <= now
    }

    /// The raw secret bytes.
    fn secret_bytes(&self) -> Result<Vec<u8>> {
        canonical::decode_base64url("secret", &self.secret)
            .map_err(|_| CryptoError::MalformedInvite)
    }
}

/// The values an invite proof commits to.
///
/// Built in one place so the prover and the verifier cannot disagree about
/// what is being authenticated, which is the classic way a MAC check ends up
/// verifying something other than what it protects.
fn proof_input(
    invite: &Invite,
    joiner_principal_public_key: &str,
    device_certificate_hash: &str,
) -> Result<Vec<u8>> {
    let bound = serde_json::json!({
        "domain": hrc_protocol::domain::INVITE_PROOF,
        "channelId": invite.channel_id,
        "inviteId": invite.invite_id,
        "principalPublicKey": joiner_principal_public_key,
        "deviceCertificateHash": device_certificate_hash,
    });

    Ok(canonical::to_canonical_bytes(&bound)?)
}

/// Computes the invite proof a join request carries (PRD section 18.0.3).
///
/// HMAC-SHA-256 under the one-time invite secret, over the canonical
/// encoding of the values the request binds to. The proof demonstrates
/// possession of the secret without publishing it, and it is useless for any
/// other join because it commits to this joiner's key and certificate.
pub fn invite_proof(
    invite: &Invite,
    joiner_principal_public_key: &str,
    device_certificate_hash: &str,
) -> Result<String> {
    let message = proof_input(invite, joiner_principal_public_key, device_certificate_hash)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(&invite.secret_bytes()?)
        .map_err(|_| CryptoError::MalformedInvite)?;
    mac.update(&message);

    Ok(canonical::encode_base64url(&mac.finalize().into_bytes()))
}

/// Verifies an invite proof.
///
/// The tag comparison goes through `verify_slice`, which is constant time.
/// Comparing the base64 strings with `==` would leak how much of a guessed
/// proof was correct.
pub fn verify_invite_proof(
    invite: &Invite,
    joiner_principal_public_key: &str,
    device_certificate_hash: &str,
    proof: &str,
) -> Result<()> {
    let provided = canonical::decode_base64url("inviteProof", proof)
        .map_err(|_| CryptoError::InviteProofInvalid)?;

    let message = proof_input(invite, joiner_principal_public_key, device_certificate_hash)?;

    let mut mac = Hmac::<Sha256>::new_from_slice(&invite.secret_bytes()?)
        .map_err(|_| CryptoError::MalformedInvite)?;
    mac.update(&message);

    mac.verify_slice(&provided)
        .map_err(|_| CryptoError::InviteProofInvalid)
}

/// A safety phrase and the wordlist it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafetyPhrase {
    /// The words, in order.
    pub words: Vec<&'static str>,
    /// Identifier of the wordlist used.
    pub wordlist: &'static str,
}

impl std::fmt::Display for SafetyPhrase {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.words.join(" "))
    }
}

/// Derives the enrollment safety phrase (PRD section 18.0.3).
///
/// Both sides compute this from public material only. An attacker who
/// substitutes either identity, or points the joiner at a different channel,
/// changes the phrase — which is the whole point of comparing it out of
/// band.
pub fn safety_phrase(
    channel_id: &str,
    invite_id: &str,
    admin_principal_public_key: &str,
    joiner_principal_public_key: &str,
    joiner_device_certificate_hash: &str,
) -> Result<SafetyPhrase> {
    let bound = serde_json::json!({
        "channelId": channel_id,
        "inviteId": invite_id,
        "adminPrincipalPublicKey": admin_principal_public_key,
        "joinerPrincipalPublicKey": joiner_principal_public_key,
        "joinerDeviceCertificateHash": joiner_device_certificate_hash,
    });

    let mut input = hrc_protocol::domain::SAFETY_PHRASE.as_bytes().to_vec();
    input.push(0x00);
    input.extend_from_slice(&canonical::to_canonical_bytes(&bound)?);

    let digest = <Sha256 as sha2::Digest>::digest(&input);
    Ok(phrase_from_digest(&digest))
}

/// Turns a digest into six words.
///
/// The first 77 bits become one integer, then six base-7776 digits. The
/// wordlist has 7776 entries, so 7776^6 is just over 2^77 and the mapping
/// consumes the bits without discarding any.
fn phrase_from_digest(digest: &[u8]) -> SafetyPhrase {
    let words = wordlist();
    debug_assert_eq!(words.len(), 7776, "the EFF long wordlist has 7776 entries");

    // Ten bytes is 80 bits; drop the low 3 to leave exactly 77.
    let mut value: u128 = 0;
    for byte in &digest[..10] {
        value = (value << 8) | u128::from(*byte);
    }
    value >>= 80 - PHRASE_BITS;

    let base = words.len() as u128;
    let mut indices = [0usize; PHRASE_WORDS];
    for slot in indices.iter_mut().rev() {
        *slot = (value % base) as usize;
        value /= base;
    }

    SafetyPhrase {
        words: indices.iter().map(|index| words[*index]).collect(),
        wordlist: WORDLIST_ID,
    }
}

#[cfg(test)]
mod tests;
