//! The join request published by an enrolling peer.
//!
//! PRD section 18.0.3. A join request is the one object a non-member
//! produces, so it is the one object whose signer cannot be resolved from the
//! roster: it carries the principal key it is signed under. That is not a
//! weakness, because the request authorizes nothing by itself. What ties it
//! to the channel is the invite proof, which only someone holding the
//! administrator's one-time secret can produce, and what ties it to a human
//! is the safety phrase both sides compare out of band.
//!
//! The payload therefore commits to everything an administrator decides on:
//! the channel, the invite, the principal key, and the device certificate.
//! Change any of them and both the proof and the phrase change.

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::control::DeviceCertificate;
use crate::error::{ProtocolError, Result};

/// What a joiner asks for, signed by their principal key.
///
/// The certificate travels as one signed object rather than as a payload and
/// a detached signature, so verifying "this principal vouched for this
/// device" is the same code path here as in genesis and in control entries
/// (decision DEC-020).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JoinRequestPayload {
    /// Protocol version.
    pub version: u32,
    /// The channel being joined.
    #[serde(rename = "channelId")]
    pub channel_id: String,
    /// The invite being redeemed.
    #[serde(rename = "inviteId")]
    pub invite_id: String,
    /// Unpadded base64url Ed25519 principal signing key of the joiner.
    #[serde(rename = "principalPublicKey")]
    pub principal_public_key: String,
    /// The joiner's first device, certified by the principal key above.
    #[serde(rename = "deviceCertificate")]
    pub device_certificate: DeviceCertificate,
    /// HMAC-SHA-256 proof of possession of the invite secret.
    #[serde(rename = "inviteProof")]
    pub invite_proof: String,
    /// RFC 3339 UTC creation time.
    #[serde(rename = "createdAt")]
    pub created_at: String,
}

impl JoinRequestPayload {
    /// Checks the invariants that do not need any key material.
    ///
    /// Running these first means the expensive verifications never see a
    /// request that was malformed to begin with.
    pub fn validate_shape(&self) -> Result<()> {
        if self.version != crate::PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                found: self.version,
                expected: crate::PROTOCOL_VERSION,
            });
        }

        canonical::expect_hex_digest("channelId", &self.channel_id, 32)?;

        for (field, value) in [
            ("inviteId", &self.invite_id),
            ("principalPublicKey", &self.principal_public_key),
            ("inviteProof", &self.invite_proof),
            ("createdAt", &self.created_at),
        ] {
            if value.is_empty() {
                return Err(ProtocolError::MissingField { field });
            }
        }

        Ok(())
    }

    /// The digest the invite proof and the safety phrase both commit to.
    pub fn device_certificate_hash(&self) -> Result<String> {
        certificate_hash(&self.device_certificate)
    }
}

/// SHA-256 of the JCS encoding of a whole device certificate.
///
/// Both sides derive it from the same function so that a joiner and an
/// administrator cannot disagree about what was committed to — the classic
/// way a proof ends up authenticating something adjacent to what it protects.
pub fn certificate_hash(certificate: &DeviceCertificate) -> Result<String> {
    canonical::canonical_sha256_hex(certificate)
}
