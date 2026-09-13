//! Genesis and the append-only control log.
//!
//! A channel's identity is its genesis object: the channel ID is the SHA-256
//! of the canonical genesis payload, so the channel cannot be renamed,
//! re-parented, or handed a different initial administrator without becoming
//! a different channel (PRD section 18.0.1).
//!
//! Every later membership or policy change is a control entry in a hash
//! chain rooted at genesis (PRD section 18.0.2). Each entry names its
//! sequence number and the hash of its predecessor, which is what makes a
//! rewritten or forked history detectable rather than merely unlikely.
//!
//! This module owns the shapes and the derivations. Deciding whether a given
//! entry is *authorized* — whether its signer held authority in the
//! preceding state — is roster evaluation, and belongs to the core.

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::error::{ProtocolError, Result};
use crate::identity::DeviceCertificatePayload;
use crate::signed::SignedObject;

/// A device certificate as it travels inside control objects.
///
/// Genesis and control entries both carry devices in this one envelope
/// rather than in two different shapes, so a verifier has a single code path
/// for "check that this principal vouched for this device" (decision
/// DEC-020).
pub type DeviceCertificate = SignedObject<DeviceCertificatePayload>;

/// Where a channel's objects live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TransportLocator {
    /// Transport kind, for example `git`.
    pub kind: String,
    /// Provider-specific locator, for example `owner/name`.
    pub locator: String,
}

/// A principal's public material and the devices it vouches for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PrincipalMaterial {
    /// Stable principal identifier.
    #[serde(rename = "principalId")]
    pub principal_id: String,
    /// Unpadded base64url Ed25519 principal signing key.
    #[serde(rename = "principalSigningKey")]
    pub principal_signing_key: String,
    /// Devices certified by this principal.
    pub devices: Vec<DeviceCertificate>,
}

/// The root object that defines a channel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenesisPayload {
    /// Protocol version.
    pub version: u32,
    /// RFC 3339 UTC creation time.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// The administrator present at channel creation.
    #[serde(rename = "initialAdmin")]
    pub initial_admin: PrincipalMaterial,
    /// Where the channel's objects live.
    pub transport: TransportLocator,
    /// Channel policy. Opaque here; interpreted by the core.
    pub policy: serde_json::Value,
}

impl GenesisPayload {
    /// Derives the channel ID: the SHA-256 of the canonical genesis payload.
    pub fn channel_id(&self) -> Result<String> {
        canonical::canonical_sha256_hex(self)
    }
}

/// A membership or policy change, and the data it carries.
///
/// Serialized adjacently so that the wire form is the `operation` and `body`
/// pair the PRD specifies, while Rust still gets one exhaustive type. An
/// unrecognized operation fails to parse rather than being ignored: a
/// participant that cannot understand a roster change must not carry on as
/// though the roster were unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operation", content = "body", rename_all = "snake_case")]
pub enum ControlOperation {
    /// Authorize a single enrollment.
    CreateInvite {
        /// Invite identifier. The secret itself is never published.
        #[serde(rename = "inviteId")]
        invite_id: String,
        /// RFC 3339 UTC expiry.
        #[serde(rename = "expiresAt")]
        expires_at: String,
    },
    /// Withdraw an unused invite.
    RevokeInvite {
        /// Invite identifier.
        #[serde(rename = "inviteId")]
        invite_id: String,
    },
    /// Admit a principal and its initial devices.
    AddMember {
        /// The invite this admission consumes.
        ///
        /// Naming it here is what makes single use (PRD section 15.1)
        /// auditable: without it, nothing in the published record says which
        /// authorization a member was admitted under, so a replayed join
        /// request could be admitted twice and no reader of the control log
        /// could tell. See decision DEC-039.
        #[serde(rename = "inviteId")]
        invite_id: String,
        /// The joining principal.
        member: PrincipalMaterial,
    },
    /// Remove a principal and all of its devices.
    RemoveMember {
        /// The departing principal.
        #[serde(rename = "principalId")]
        principal_id: String,
    },
    /// Add a device to an existing member.
    AddDevice {
        /// Certificate signed by the owning principal.
        certificate: DeviceCertificate,
    },
    /// Revoke one device without removing its principal.
    RevokeDevice {
        /// Device to revoke.
        #[serde(rename = "deviceId")]
        device_id: String,
    },
    /// Replace a device's keys with freshly generated ones.
    RotateDeviceKey {
        /// Device being replaced.
        #[serde(rename = "previousDeviceId")]
        previous_device_id: String,
        /// Certificate for the replacement device.
        certificate: DeviceCertificate,
    },
    /// Replace a principal's signing key.
    RotatePrincipalKey {
        /// Principal being rekeyed.
        #[serde(rename = "principalId")]
        principal_id: String,
        /// The new principal signing key.
        #[serde(rename = "principalSigningKey")]
        principal_signing_key: String,
    },
    /// Change channel policy.
    UpdatePolicy {
        /// The replacement policy document.
        policy: serde_json::Value,
    },
    /// Move the channel to a fresh repository.
    RolloverRepository {
        /// Where the channel continues.
        transport: TransportLocator,
    },
}

impl ControlOperation {
    /// Whether this operation changes who can read future messages.
    ///
    /// Operations that do advance the roster epoch are exactly those that
    /// change the active device set; the epoch is what later messages are
    /// validated against, so getting this list wrong would let a removed
    /// device stay addressable.
    pub fn advances_epoch(&self) -> bool {
        match self {
            ControlOperation::AddMember { .. }
            | ControlOperation::RemoveMember { .. }
            | ControlOperation::AddDevice { .. }
            | ControlOperation::RevokeDevice { .. }
            | ControlOperation::RotateDeviceKey { .. }
            | ControlOperation::RotatePrincipalKey { .. } => true,

            ControlOperation::CreateInvite { .. }
            | ControlOperation::RevokeInvite { .. }
            | ControlOperation::UpdatePolicy { .. }
            | ControlOperation::RolloverRepository { .. } => false,
        }
    }

    /// The wire name of this operation.
    pub fn name(&self) -> &'static str {
        match self {
            ControlOperation::CreateInvite { .. } => "create_invite",
            ControlOperation::RevokeInvite { .. } => "revoke_invite",
            ControlOperation::AddMember { .. } => "add_member",
            ControlOperation::RemoveMember { .. } => "remove_member",
            ControlOperation::AddDevice { .. } => "add_device",
            ControlOperation::RevokeDevice { .. } => "revoke_device",
            ControlOperation::RotateDeviceKey { .. } => "rotate_device_key",
            ControlOperation::RotatePrincipalKey { .. } => "rotate_principal_key",
            ControlOperation::UpdatePolicy { .. } => "update_policy",
            ControlOperation::RolloverRepository { .. } => "rollover_repository",
        }
    }
}

/// One entry in the append-only control log.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlEntryPayload {
    /// Protocol version.
    pub version: u32,
    /// Channel this entry belongs to.
    #[serde(rename = "channelId")]
    pub channel_id: String,
    /// Position in the chain. Genesis is zero; entries start at one.
    pub sequence: u64,
    /// Hash of the preceding entry's payload.
    #[serde(rename = "previousHash")]
    pub previous_hash: String,
    /// Roster epoch established by this entry.
    pub epoch: u64,
    /// RFC 3339 UTC creation time.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// What changed.
    #[serde(flatten)]
    pub operation: ControlOperation,
}

impl ControlEntryPayload {
    /// The hash a successor entry names as its `previousHash`.
    pub fn entry_hash(&self) -> Result<String> {
        canonical::canonical_sha256_hex(self)
    }

    /// Checks the shape-level invariants that need no roster context.
    ///
    /// This is the cheap half of validation and runs first: it costs nothing
    /// and rejects malformed entries before any signature work happens.
    /// Authority and ordering against real membership are the core's job.
    pub fn validate_shape(&self) -> Result<()> {
        if self.version != crate::PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                found: self.version,
                expected: crate::PROTOCOL_VERSION,
            });
        }

        canonical::expect_hex_digest("channelId", &self.channel_id, 32)?;
        canonical::expect_hex_digest("previousHash", &self.previous_hash, 32)?;

        if self.sequence == 0 {
            // Sequence zero is genesis, which is a different object with a
            // different signature domain and cannot appear as an entry.
            return Err(ProtocolError::InvalidControlSequence { sequence: 0 });
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::DeviceDescriptor;
    use crate::signed::Signer;

    fn descriptor(seed: &str) -> DeviceDescriptor {
        DeviceDescriptor {
            version: crate::PROTOCOL_VERSION,
            principal_id: "principal-1".into(),
            encryption_recipient: format!("age1{seed}"),
            signing_key: "c2lnbmluZy1rZXk".into(),
            created_at: "2026-09-13T00:00:00Z".into(),
            expires_at: None,
            nonce: "bm9uY2UtMQ".into(),
        }
    }

    fn certificate(seed: &str) -> DeviceCertificate {
        SignedObject {
            version: crate::PROTOCOL_VERSION,
            signer: Signer {
                principal_id: "principal-1".into(),
                device_id: "device-1".into(),
            },
            payload: DeviceCertificatePayload::new(descriptor(seed)).unwrap(),
            signature: "c2lnbmF0dXJl".into(),
        }
    }

    fn genesis() -> GenesisPayload {
        GenesisPayload {
            version: crate::PROTOCOL_VERSION,
            created_at: "2026-09-13T00:00:00Z".into(),
            initial_admin: PrincipalMaterial {
                principal_id: "principal-1".into(),
                principal_signing_key: "cHJpbmNpcGFsLWtleQ".into(),
                devices: vec![certificate("admin")],
            },
            transport: TransportLocator {
                kind: "git".into(),
                locator: "owner/channel".into(),
            },
            policy: serde_json::json!({}),
        }
    }

    fn entry(
        sequence: u64,
        previous_hash: &str,
        operation: ControlOperation,
    ) -> ControlEntryPayload {
        ControlEntryPayload {
            version: crate::PROTOCOL_VERSION,
            channel_id: canonical::sha256_hex(b"channel"),
            sequence,
            previous_hash: previous_hash.to_owned(),
            epoch: 1,
            created_at: "2026-09-13T00:00:00Z".into(),
            operation,
        }
    }

    #[test]
    fn the_channel_id_is_stable() {
        assert_eq!(
            genesis().channel_id().unwrap(),
            genesis().channel_id().unwrap()
        );
        assert_eq!(genesis().channel_id().unwrap().len(), 64);
    }

    #[test]
    fn any_change_to_genesis_changes_the_channel_id() {
        let original = genesis().channel_id().unwrap();

        let mut moved = genesis();
        moved.transport.locator = "someone-else/channel".into();
        assert_ne!(moved.channel_id().unwrap(), original);

        let mut readmin = genesis();
        readmin.initial_admin.principal_signing_key = "b3RoZXIta2V5".into();
        assert_ne!(readmin.channel_id().unwrap(), original);

        let mut extra_device = genesis();
        extra_device
            .initial_admin
            .devices
            .push(certificate("second"));
        assert_ne!(extra_device.channel_id().unwrap(), original);

        let mut repolicied = genesis();
        repolicied.policy = serde_json::json!({ "readReceipts": true });
        assert_ne!(repolicied.channel_id().unwrap(), original);
    }

    #[test]
    fn control_entries_serialize_in_the_specified_shape() {
        // The wire form must be a flat object carrying `operation` and
        // `body`, matching PRD section 18.0.2 rather than a Rust-shaped
        // enum encoding.
        let payload = entry(
            1,
            &canonical::sha256_hex(b"genesis"),
            ControlOperation::RevokeDevice {
                device_id: "device-9".into(),
            },
        );

        let encoded: serde_json::Value =
            serde_json::from_str(&canonical::to_canonical_json(&payload).unwrap()).unwrap();

        assert_eq!(encoded["operation"], "revoke_device");
        assert_eq!(encoded["body"]["deviceId"], "device-9");
        assert_eq!(encoded["sequence"], 1);
        assert!(encoded.get("previousHash").is_some());
    }

    #[test]
    fn every_operation_round_trips() {
        let operations = vec![
            ControlOperation::CreateInvite {
                invite_id: "invite-1".into(),
                expires_at: "2026-09-14T00:00:00Z".into(),
            },
            ControlOperation::RevokeInvite {
                invite_id: "invite-1".into(),
            },
            ControlOperation::AddMember {
                invite_id: "invite-1".into(),
                member: PrincipalMaterial {
                    principal_id: "principal-2".into(),
                    principal_signing_key: "a2V5".into(),
                    devices: vec![certificate("member")],
                },
            },
            ControlOperation::RemoveMember {
                principal_id: "principal-2".into(),
            },
            ControlOperation::AddDevice {
                certificate: certificate("added"),
            },
            ControlOperation::RevokeDevice {
                device_id: "device-2".into(),
            },
            ControlOperation::RotateDeviceKey {
                previous_device_id: "device-2".into(),
                certificate: certificate("rotated"),
            },
            ControlOperation::RotatePrincipalKey {
                principal_id: "principal-1".into(),
                principal_signing_key: "bmV3LWtleQ".into(),
            },
            ControlOperation::UpdatePolicy {
                policy: serde_json::json!({ "readReceipts": false }),
            },
            ControlOperation::RolloverRepository {
                transport: TransportLocator {
                    kind: "git".into(),
                    locator: "owner/next".into(),
                },
            },
        ];

        for operation in operations {
            let payload = entry(1, &canonical::sha256_hex(b"prev"), operation.clone());
            let encoded = canonical::to_canonical_json(&payload).unwrap();
            let decoded: ControlEntryPayload = canonical::from_json_str(&encoded).unwrap();

            assert_eq!(decoded, payload, "{} did not round trip", operation.name());
        }
    }

    #[test]
    fn an_unknown_operation_fails_to_parse() {
        // Silently ignoring an unrecognized roster change would leave a
        // participant operating on a membership view it knows is incomplete.
        let encoded = r#"{
            "version": 1,
            "channelId": "aa",
            "sequence": 1,
            "previousHash": "bb",
            "epoch": 1,
            "createdAt": "2026-09-13T00:00:00Z",
            "operation": "grant_everything",
            "body": {}
        }"#;

        assert!(canonical::from_json_str::<ControlEntryPayload>(encoded).is_err());
    }

    #[test]
    fn membership_changes_advance_the_epoch_and_others_do_not() {
        assert!(
            ControlOperation::RevokeDevice {
                device_id: "d".into()
            }
            .advances_epoch()
        );
        assert!(
            ControlOperation::AddMember {
                invite_id: "invite-1".into(),
                member: PrincipalMaterial {
                    principal_id: "p".into(),
                    principal_signing_key: "k".into(),
                    devices: vec![],
                }
            }
            .advances_epoch()
        );
        assert!(
            !ControlOperation::CreateInvite {
                invite_id: "i".into(),
                expires_at: "2026-09-14T00:00:00Z".into()
            }
            .advances_epoch()
        );
        assert!(
            !ControlOperation::UpdatePolicy {
                policy: serde_json::json!({})
            }
            .advances_epoch()
        );
    }

    #[test]
    fn entry_hashes_chain_and_detect_alteration() {
        let first = entry(
            1,
            &canonical::sha256_hex(b"genesis"),
            ControlOperation::RevokeInvite {
                invite_id: "invite-1".into(),
            },
        );
        let hash = first.entry_hash().unwrap();

        let mut altered = first.clone();
        altered.created_at = "2026-09-13T00:00:01Z".into();
        assert_ne!(altered.entry_hash().unwrap(), hash);

        let successor = entry(
            2,
            &hash,
            ControlOperation::RevokeDevice {
                device_id: "device-2".into(),
            },
        );
        assert_eq!(successor.previous_hash, hash);
        assert_ne!(successor.entry_hash().unwrap(), hash);
    }

    #[test]
    fn shape_validation_accepts_a_well_formed_entry() {
        entry(
            1,
            &canonical::sha256_hex(b"genesis"),
            ControlOperation::RevokeDevice {
                device_id: "device-2".into(),
            },
        )
        .validate_shape()
        .unwrap();
    }

    #[test]
    fn shape_validation_rejects_sequence_zero() {
        // Sequence zero is genesis, which is signed under a different domain.
        let payload = entry(
            0,
            &canonical::sha256_hex(b"genesis"),
            ControlOperation::RevokeDevice {
                device_id: "device-2".into(),
            },
        );

        assert!(matches!(
            payload.validate_shape().unwrap_err(),
            ProtocolError::InvalidControlSequence { sequence: 0 }
        ));
    }

    #[test]
    fn shape_validation_rejects_malformed_hashes_and_versions() {
        let base = entry(
            1,
            &canonical::sha256_hex(b"genesis"),
            ControlOperation::RevokeDevice {
                device_id: "device-2".into(),
            },
        );

        let mut bad_previous = base.clone();
        bad_previous.previous_hash = "nope".into();
        assert!(matches!(
            bad_previous.validate_shape().unwrap_err(),
            ProtocolError::Hex { .. }
        ));

        let mut bad_channel = base.clone();
        bad_channel.channel_id = "nope".into();
        assert!(matches!(
            bad_channel.validate_shape().unwrap_err(),
            ProtocolError::Hex { .. }
        ));

        let mut bad_version = base;
        bad_version.version = 99;
        assert!(matches!(
            bad_version.validate_shape().unwrap_err(),
            ProtocolError::UnsupportedVersion { found: 99, .. }
        ));
    }
}
