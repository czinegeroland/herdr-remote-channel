//! Roster evaluation: turning a control chain into membership state.
//!
//! The control log is an append-only chain of signed entries. This module
//! replays that chain from genesis and answers the questions the rest of the
//! system asks of membership:
//!
//! - Which principals are members, and which devices are active?
//! - Who had authority to sign a given entry, at the moment it was signed?
//! - Which roster epoch was in force when a message was introduced?
//!
//! Two rules from PRD section 17.5 shape the design:
//!
//! 1. **Authority is evaluated against the preceding state**, never the
//!    resulting one. An entry that revokes a device must be checked against
//!    the roster in which that device was still active, or an administrator
//!    could not revoke their own compromised device.
//! 2. **Epochs advance only on membership change.** A message names the
//!    epoch it was encrypted under, and a device removed in epoch N must not
//!    be able to have messages accepted under epoch N-1 afterwards.
//!
//! The MVP detects conflicts visible in the history it is shown
//! (PRD section 17.5.2). Split-view equivocation by a repository operator is
//! out of scope and needs a transparency witness; nothing here pretends
//! otherwise.

use std::collections::BTreeMap;

use hrc_crypto::VerifyingKey;
use hrc_protocol::control::{
    ControlEntryPayload, ControlOperation, DeviceCertificate, GenesisPayload, PrincipalMaterial,
};
use hrc_protocol::domain;
use hrc_protocol::signed::SignedObject;

use crate::error::{CoreError, Result};

/// A device's participation state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceStatus {
    /// The device may send and receive.
    Active,
    /// The device was revoked, or its principal was removed.
    ///
    /// Revoked devices are retained rather than deleted: validating an old
    /// message requires knowing who a since-revoked signer was, and in which
    /// epoch they stopped being authorized.
    Revoked {
        /// Epoch in which the revocation took effect.
        epoch: u64,
    },
}

/// A device known to the roster.
#[derive(Debug, Clone)]
pub struct RosterDevice {
    /// Device identifier, derived from its descriptor.
    pub device_id: String,
    /// Principal that owns the device.
    pub principal_id: String,
    /// age recipient used to encrypt to this device.
    pub encryption_recipient: String,
    /// Ed25519 key this device signs with.
    pub signing_key: String,
    /// Epoch in which the device became active.
    pub added_in_epoch: u64,
    /// Current participation state.
    pub status: DeviceStatus,
}

impl RosterDevice {
    /// Whether the device was active during `epoch`.
    ///
    /// A device is usable in the epoch it was added and every epoch up to,
    /// but not including, the one that revoked it.
    pub fn was_active_in(&self, epoch: u64) -> bool {
        if epoch < self.added_in_epoch {
            return false;
        }

        match self.status {
            DeviceStatus::Active => true,
            DeviceStatus::Revoked {
                epoch: revoked_epoch,
            } => epoch < revoked_epoch,
        }
    }
}

/// A principal known to the roster.
#[derive(Debug, Clone)]
pub struct RosterMember {
    /// Principal identifier.
    pub principal_id: String,
    /// Current principal signing key.
    pub principal_signing_key: String,
    /// Whether this principal may sign control entries.
    ///
    /// The MVP supports exactly one administrator (decision DEC-009): the
    /// principal named in genesis.
    pub is_administrator: bool,
    /// Whether the principal is still a member.
    pub is_active: bool,
}

/// Membership state produced by replaying a control chain.
#[derive(Debug, Clone)]
pub struct Roster {
    channel_id: String,
    epoch: u64,
    sequence: u64,
    head_hash: String,
    members: BTreeMap<String, RosterMember>,
    devices: BTreeMap<String, RosterDevice>,
}

impl Roster {
    /// Builds the initial roster from a verified genesis object.
    ///
    /// Genesis is signed by an administrator *device*, like every other
    /// signed object: the envelope names a device, and the certificate chain
    /// is what ties that device to the principal. So the order is
    /// certificates first, then the genesis signature against the signing
    /// device's key.
    ///
    /// All of this is self-asserting — genesis declares the very key that
    /// vouches for it. That is why the channel ID is a hash of the whole
    /// payload: a participant joins a channel *identified by* that hash, so
    /// substituting a different administrator produces a different channel
    /// rather than a hijacked one.
    ///
    /// The head hash starts as the channel ID, which is the hash of the
    /// genesis payload, so the first control entry chains to genesis the same
    /// way every later entry chains to its predecessor.
    pub fn from_genesis(genesis: &SignedObject<GenesisPayload>) -> Result<Self> {
        let payload = &genesis.payload;
        let channel_id = payload.channel_id()?;

        if genesis.signer.principal_id != payload.initial_admin.principal_id {
            return Err(CoreError::UnauthorizedSigner {
                principal_id: genesis.signer.principal_id.clone(),
            });
        }

        let mut roster = Self {
            channel_id: channel_id.clone(),
            epoch: 0,
            sequence: 0,
            head_hash: channel_id,
            members: BTreeMap::new(),
            devices: BTreeMap::new(),
        };

        // Admitting the principal verifies every device certificate against
        // the principal key genesis declares.
        roster.admit_principal(&payload.initial_admin, true, 0)?;

        // The genesis signer must be one of the devices genesis certifies,
        // otherwise nothing ties the signature to a device the roster knows.
        let device = roster
            .devices
            .get(&genesis.signer.device_id)
            .ok_or_else(|| CoreError::UnknownDevice {
                device_id: genesis.signer.device_id.clone(),
            })?;

        let device_key = VerifyingKey::from_base64url("signingKey", &device.signing_key)?;
        device_key.verify_object(domain::GENESIS, genesis)?;

        Ok(roster)
    }

    /// Applies the next control entry in the chain.
    ///
    /// Every check that can reject the entry happens before any state is
    /// mutated, so a rejected entry leaves the roster exactly as it was.
    pub fn apply(&mut self, entry: &SignedObject<ControlEntryPayload>) -> Result<()> {
        let payload = &entry.payload;
        payload.validate_shape()?;

        if payload.channel_id != self.channel_id {
            return Err(CoreError::WrongChannel {
                expected: self.channel_id.clone(),
                found: payload.channel_id.clone(),
            });
        }

        if payload.sequence != self.sequence + 1 {
            return Err(CoreError::SequenceOutOfOrder {
                expected: self.sequence + 1,
                found: payload.sequence,
            });
        }

        if payload.previous_hash != self.head_hash {
            return Err(CoreError::BrokenChain {
                expected: self.head_hash.clone(),
                found: payload.previous_hash.clone(),
            });
        }

        let expected_epoch = if payload.operation.advances_epoch() {
            self.epoch + 1
        } else {
            self.epoch
        };
        if payload.epoch != expected_epoch {
            return Err(CoreError::EpochOutOfOrder {
                expected: expected_epoch,
                found: payload.epoch,
            });
        }

        // Authority is evaluated against the state *before* this entry, so an
        // administrator can revoke their own compromised device.
        self.verify_signer_authority(entry)?;

        let next_hash = payload.entry_hash()?;
        self.apply_operation(&payload.operation, expected_epoch)?;

        self.sequence = payload.sequence;
        self.epoch = expected_epoch;
        self.head_hash = next_hash;
        Ok(())
    }

    /// Checks that the signing device belonged to an authorized administrator.
    fn verify_signer_authority(&self, entry: &SignedObject<ControlEntryPayload>) -> Result<()> {
        let device =
            self.devices
                .get(&entry.signer.device_id)
                .ok_or_else(|| CoreError::UnknownDevice {
                    device_id: entry.signer.device_id.clone(),
                })?;

        if device.principal_id != entry.signer.principal_id {
            return Err(CoreError::UnauthorizedSigner {
                principal_id: entry.signer.principal_id.clone(),
            });
        }

        if !device.was_active_in(self.epoch) {
            return Err(CoreError::RevokedSigner {
                device_id: device.device_id.clone(),
            });
        }

        let member = self.members.get(&device.principal_id).ok_or_else(|| {
            CoreError::UnauthorizedSigner {
                principal_id: device.principal_id.clone(),
            }
        })?;

        if !member.is_active || !member.is_administrator {
            return Err(CoreError::UnauthorizedSigner {
                principal_id: member.principal_id.clone(),
            });
        }

        let key = VerifyingKey::from_base64url("signingKey", &device.signing_key)?;
        key.verify_object(domain::CONTROL, entry)?;
        Ok(())
    }

    /// Mutates state for an already-authorized operation.
    fn apply_operation(&mut self, operation: &ControlOperation, epoch: u64) -> Result<()> {
        match operation {
            ControlOperation::AddMember { member } => {
                if self.members.contains_key(&member.principal_id) {
                    return Err(CoreError::DuplicateMember {
                        principal_id: member.principal_id.clone(),
                    });
                }
                self.admit_principal(member, false, epoch)?;
            }

            ControlOperation::RemoveMember { principal_id } => {
                let member =
                    self.members
                        .get_mut(principal_id)
                        .ok_or_else(|| CoreError::UnknownMember {
                            principal_id: principal_id.clone(),
                        })?;
                if member.is_administrator {
                    // With one administrator (DEC-009), removing them would
                    // leave the channel unable to change membership again.
                    return Err(CoreError::CannotRemoveAdministrator);
                }
                member.is_active = false;

                for device in self.devices.values_mut() {
                    if &device.principal_id == principal_id && device.status == DeviceStatus::Active
                    {
                        device.status = DeviceStatus::Revoked { epoch };
                    }
                }
            }

            ControlOperation::AddDevice { certificate } => {
                self.admit_device(certificate, epoch)?;
            }

            ControlOperation::RevokeDevice { device_id } => {
                self.revoke_device(device_id, epoch)?;
            }

            ControlOperation::RotateDeviceKey {
                previous_device_id,
                certificate,
            } => {
                self.revoke_device(previous_device_id, epoch)?;
                self.admit_device(certificate, epoch)?;
            }

            ControlOperation::RotatePrincipalKey {
                principal_id,
                principal_signing_key,
            } => {
                let member =
                    self.members
                        .get_mut(principal_id)
                        .ok_or_else(|| CoreError::UnknownMember {
                            principal_id: principal_id.clone(),
                        })?;
                member.principal_signing_key = principal_signing_key.clone();
            }

            // Invites and policy do not change the device set. Invite state
            // and policy interpretation live outside the roster.
            ControlOperation::CreateInvite { .. }
            | ControlOperation::RevokeInvite { .. }
            | ControlOperation::UpdatePolicy { .. }
            | ControlOperation::RolloverRepository { .. } => {}
        }

        Ok(())
    }

    /// Adds a principal and the devices it certifies.
    fn admit_principal(
        &mut self,
        material: &PrincipalMaterial,
        is_administrator: bool,
        epoch: u64,
    ) -> Result<()> {
        // Validate every certificate before inserting anything, so a
        // principal is never half-added.
        for certificate in &material.devices {
            self.check_certificate(certificate, &material.principal_id, material)?;
        }

        self.members.insert(
            material.principal_id.clone(),
            RosterMember {
                principal_id: material.principal_id.clone(),
                principal_signing_key: material.principal_signing_key.clone(),
                is_administrator,
                is_active: true,
            },
        );

        for certificate in &material.devices {
            self.insert_device(certificate, epoch);
        }

        Ok(())
    }

    /// Adds one device to an existing member.
    fn admit_device(&mut self, certificate: &DeviceCertificate, epoch: u64) -> Result<()> {
        let principal_id = certificate.payload.descriptor.principal_id.clone();
        let member = self
            .members
            .get(&principal_id)
            .filter(|member| member.is_active)
            .ok_or_else(|| CoreError::UnknownMember {
                principal_id: principal_id.clone(),
            })?
            .clone();

        let material = PrincipalMaterial {
            principal_id: member.principal_id.clone(),
            principal_signing_key: member.principal_signing_key.clone(),
            devices: Vec::new(),
        };
        self.check_certificate(certificate, &principal_id, &material)?;

        if self.devices.contains_key(&certificate.payload.device_id) {
            return Err(CoreError::DuplicateDevice {
                device_id: certificate.payload.device_id.clone(),
            });
        }

        self.insert_device(certificate, epoch);
        Ok(())
    }

    /// Verifies that a principal vouched for a device.
    fn check_certificate(
        &self,
        certificate: &DeviceCertificate,
        expected_principal: &str,
        material: &PrincipalMaterial,
    ) -> Result<()> {
        // Recompute the device ID before trusting the signature: a signature
        // over a payload whose ID does not describe its own descriptor
        // proves nothing useful (PRD 18.0.1).
        certificate.payload.verify_device_id()?;

        if certificate.payload.descriptor.principal_id != expected_principal {
            return Err(CoreError::CertificatePrincipalMismatch {
                expected: expected_principal.to_owned(),
                found: certificate.payload.descriptor.principal_id.clone(),
            });
        }

        let key =
            VerifyingKey::from_base64url("principalSigningKey", &material.principal_signing_key)?;
        key.verify_object(domain::DEVICE_CERTIFICATE, certificate)?;
        Ok(())
    }

    /// Inserts a validated device.
    fn insert_device(&mut self, certificate: &DeviceCertificate, epoch: u64) {
        let descriptor = &certificate.payload.descriptor;
        self.devices.insert(
            certificate.payload.device_id.clone(),
            RosterDevice {
                device_id: certificate.payload.device_id.clone(),
                principal_id: descriptor.principal_id.clone(),
                encryption_recipient: descriptor.encryption_recipient.clone(),
                signing_key: descriptor.signing_key.clone(),
                added_in_epoch: epoch,
                status: DeviceStatus::Active,
            },
        );
    }

    /// Marks a device revoked as of `epoch`.
    fn revoke_device(&mut self, device_id: &str, epoch: u64) -> Result<()> {
        let device = self
            .devices
            .get_mut(device_id)
            .ok_or_else(|| CoreError::UnknownDevice {
                device_id: device_id.to_owned(),
            })?;

        if device.status != DeviceStatus::Active {
            return Err(CoreError::AlreadyRevoked {
                device_id: device_id.to_owned(),
            });
        }

        device.status = DeviceStatus::Revoked { epoch };
        Ok(())
    }

    /// The channel this roster belongs to.
    pub fn channel_id(&self) -> &str {
        &self.channel_id
    }

    /// The current roster epoch.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The sequence number of the last applied entry.
    pub fn sequence(&self) -> u64 {
        self.sequence
    }

    /// The hash a successor entry must name as its `previousHash`.
    pub fn head_hash(&self) -> &str {
        &self.head_hash
    }

    /// Every device the roster has ever known, revoked ones included.
    pub fn devices(&self) -> impl Iterator<Item = &RosterDevice> {
        self.devices.values()
    }

    /// Devices that may currently send and receive.
    pub fn active_devices(&self) -> impl Iterator<Item = &RosterDevice> {
        self.devices
            .values()
            .filter(|device| device.status == DeviceStatus::Active)
    }

    /// Members, active and removed.
    pub fn members(&self) -> impl Iterator<Item = &RosterMember> {
        self.members.values()
    }

    /// Looks up a device by ID, whatever its status.
    pub fn device(&self, device_id: &str) -> Option<&RosterDevice> {
        self.devices.get(device_id)
    }

    /// Whether `device_id` was authorized to act during `epoch`.
    ///
    /// This is the check that rejects a message introduced after a device was
    /// revoked but claiming an earlier epoch (PRD section 17.5, HRC-SEC-013).
    pub fn was_authorized_in(&self, device_id: &str, epoch: u64) -> bool {
        self.devices
            .get(device_id)
            .is_some_and(|device| device.was_active_in(epoch))
    }
}

#[cfg(test)]
mod tests;
