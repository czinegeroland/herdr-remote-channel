//! Device descriptors, device IDs, and device certificates.
//!
//! PRD section 18.0.1 makes a device ID the SHA-256 of the JCS encoding of
//! its own descriptor. That self-certifying construction is what lets a
//! verifier recompute the ID instead of trusting the one it was handed: a
//! descriptor altered after issue no longer hashes to the ID the certificate
//! was signed over.

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::error::{ProtocolError, Result};

/// The public key material and metadata that define a device.
///
/// Field names are the wire names, and the order here is irrelevant because
/// canonicalization sorts them. Every field is part of the device ID, so
/// adding one is a protocol change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceDescriptor {
    /// Descriptor schema version.
    pub version: u32,
    /// Principal this device belongs to.
    #[serde(rename = "principalId")]
    pub principal_id: String,
    /// age X25519 recipient used to encrypt to this device.
    #[serde(rename = "encryptionRecipient")]
    pub encryption_recipient: String,
    /// Unpadded base64url Ed25519 verifying key.
    #[serde(rename = "signingKey")]
    pub signing_key: String,
    /// RFC 3339 UTC creation time.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// RFC 3339 UTC expiry, or `null` when the device does not expire.
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
    /// Unpadded base64url random value.
    ///
    /// Two devices could otherwise share a descriptor, and therefore an ID,
    /// if they were created in the same second with the same keys.
    pub nonce: String,
}

impl DeviceDescriptor {
    /// Derives this descriptor's device ID.
    pub fn device_id(&self) -> Result<String> {
        canonical::canonical_sha256_hex(self)
    }
}

/// A descriptor bound to the ID derived from it.
///
/// This is the payload the principal key signs, under domain
/// `hrc/v1/device-certificate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeviceCertificatePayload {
    /// SHA-256 of the JCS encoding of `descriptor`.
    #[serde(rename = "deviceId")]
    pub device_id: String,
    /// The device's public material.
    pub descriptor: DeviceDescriptor,
}

impl DeviceCertificatePayload {
    /// Builds a certificate payload, deriving the device ID from `descriptor`.
    pub fn new(descriptor: DeviceDescriptor) -> Result<Self> {
        Ok(Self {
            device_id: descriptor.device_id()?,
            descriptor,
        })
    }

    /// Recomputes the device ID and checks it against the carried value.
    ///
    /// PRD section 18.0.1 requires this before the principal signature is
    /// checked: a signature over a payload whose ID does not describe its own
    /// descriptor proves nothing useful.
    pub fn verify_device_id(&self) -> Result<()> {
        canonical::expect_hex_digest("deviceId", &self.device_id, 32)?;

        if self.descriptor.device_id()? == self.device_id {
            Ok(())
        } else {
            Err(ProtocolError::DerivedMismatch { field: "deviceId" })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn descriptor() -> DeviceDescriptor {
        DeviceDescriptor {
            version: crate::PROTOCOL_VERSION,
            principal_id: "principal-1".into(),
            encryption_recipient: "age1exampleexamplerecipient".into(),
            signing_key: "c2lnbmluZy1rZXk".into(),
            created_at: "2026-09-13T00:00:00Z".into(),
            expires_at: None,
            nonce: "bm9uY2UtMQ".into(),
        }
    }

    #[test]
    fn device_ids_are_stable_across_runs() {
        // Pinned so a change to the descriptor shape or to canonicalization
        // shows up as a failing test rather than as silently rotated IDs.
        assert_eq!(
            descriptor().device_id().unwrap(),
            "7073e48cf0fe6a40dca70a2b874f20d94bbbaf0484eac99a92b8fc79d55faa53"
        );
    }

    #[test]
    fn a_changed_field_changes_the_device_id() {
        let original = descriptor().device_id().unwrap();

        let mut rotated = descriptor();
        rotated.signing_key = "b3RoZXIta2V5".into();
        assert_ne!(rotated.device_id().unwrap(), original);

        let mut renonced = descriptor();
        renonced.nonce = "bm9uY2UtMg".into();
        assert_ne!(renonced.device_id().unwrap(), original);

        let mut expiring = descriptor();
        expiring.expires_at = Some("2027-09-13T00:00:00Z".into());
        assert_ne!(expiring.device_id().unwrap(), original);
    }

    #[test]
    fn certificates_carry_the_derived_id() {
        let certificate = DeviceCertificatePayload::new(descriptor()).unwrap();
        assert_eq!(certificate.device_id, descriptor().device_id().unwrap());
        certificate.verify_device_id().unwrap();
    }

    #[test]
    fn a_tampered_descriptor_fails_verification() {
        let mut certificate = DeviceCertificatePayload::new(descriptor()).unwrap();
        certificate.descriptor.encryption_recipient = "age1attackerrecipient".into();

        let error = certificate.verify_device_id().unwrap_err();
        assert!(
            matches!(error, ProtocolError::DerivedMismatch { field: "deviceId" }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_malformed_device_id_is_rejected_before_hashing() {
        let mut certificate = DeviceCertificatePayload::new(descriptor()).unwrap();
        certificate.device_id = certificate.device_id.to_uppercase();

        assert!(matches!(
            certificate.verify_device_id().unwrap_err(),
            ProtocolError::Hex { .. }
        ));
    }
}
