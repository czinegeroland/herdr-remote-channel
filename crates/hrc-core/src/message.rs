//! Sealing and opening messages.
//!
//! This is where the roster, the cryptography, and the envelope meet. Two
//! operations:
//!
//! - [`seal`] builds a signed, encrypted message addressed to the devices
//!   the roster says are active, and commits to exactly that set.
//! - [`open`] runs the normative validation order of PRD section 18.0 and
//!   hands back a body that is **quarantined**, not delivered.
//!
//! The validation order is not an implementation preference. Each step
//! establishes what the next is allowed to assume, and running them out of
//! order means trusting something that has not been checked yet: verifying
//! a signature before resolving the signer from the roster would verify
//! against a key the message itself supplied.

use hrc_crypto::{DeviceIdentity, DeviceRecipient, SigningKey, VerifyingKey, encrypt_to};
use hrc_protocol::canonical;
use hrc_protocol::domain;
use hrc_protocol::message::MessageEnvelope;
use hrc_protocol::signed::{SignedObject, Signer};

use crate::error::{CoreError, Result};
use crate::roster::Roster;

/// A message that has been decrypted and fully validated, but not delivered.
///
/// Holding one of these does **not** authorize showing it to an agent. The
/// prompt gate of PRD section 19 is a separate decision made by a human, and
/// the type name is a reminder of that: this is quarantined content.
#[derive(Debug, Clone)]
pub struct QuarantinedMessage {
    /// The validated envelope.
    pub envelope: MessageEnvelope,
    /// The verified sender principal, resolved from the roster.
    pub sender_principal: String,
    /// The verified sender device, resolved from the roster.
    pub sender_device: String,
    /// SHA-256 of the ciphertext this message arrived as.
    ///
    /// An approval is bound to this digest, so approving a message and
    /// delivering a different one is not possible (PRD section 19.5).
    pub ciphertext_sha256: String,
}

/// Builds a signed, encrypted message for the active recipient devices.
///
/// The recipient set is derived from the roster rather than supplied by the
/// caller, and the envelope commits to exactly the set used to build the age
/// recipients. PRD requirement HRC-SEC-016 is otherwise unenforceable: a
/// caller that could pass a list separately could commit to one set and
/// encrypt to another.
#[allow(clippy::too_many_arguments)]
pub fn seal(
    roster: &Roster,
    signing_key: &SigningKey,
    sender: Signer,
    mut envelope: MessageEnvelope,
) -> Result<Vec<u8>> {
    if envelope.channel_id != *roster.channel_id() {
        return Err(CoreError::WrongChannel {
            expected: roster.channel_id().to_owned(),
            found: envelope.channel_id.clone(),
        });
    }

    if envelope.roster_epoch != roster.epoch() {
        // Encrypting under a stale epoch would address devices the roster has
        // since revoked (PRD section 17.5).
        return Err(CoreError::EpochOutOfOrder {
            expected: roster.epoch(),
            found: envelope.roster_epoch,
        });
    }

    if !roster.was_authorized_in(&sender.device_id, roster.epoch()) {
        return Err(CoreError::RevokedSigner {
            device_id: sender.device_id.clone(),
        });
    }

    // Build the recipient set from the roster, then commit to it. These two
    // must come from one source or the commitment means nothing.
    let recipients = intended_recipients(roster, &envelope)?;
    envelope.recipients = hrc_protocol::RecipientDevices::new(
        recipients.iter().map(|device| device.device_id.clone()),
    )?;

    // Pad after the recipient set is final, since it changes the length.
    envelope.padding = String::new();
    let unpadded = canonical::to_canonical_bytes(&envelope)?.len();
    envelope.padding = hrc_protocol::message::padding_for(unpadded)?;

    envelope.validate_shape()?;

    let signed = signing_key.sign_object(domain::MESSAGE, sender, envelope)?;
    let plaintext = canonical::to_canonical_bytes(&signed)?;

    let age_recipients = recipients
        .iter()
        .map(|device| DeviceRecipient::parse(&device.encryption_recipient))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    Ok(encrypt_to(&age_recipients, &plaintext)?)
}

/// Resolves the devices a message should be encrypted to.
///
/// Direct messages go to every active device of each addressed principal;
/// an empty principal list is a channel broadcast to every active device.
fn intended_recipients<'a>(
    roster: &'a Roster,
    envelope: &MessageEnvelope,
) -> Result<Vec<&'a crate::roster::RosterDevice>> {
    let devices: Vec<&crate::roster::RosterDevice> = if envelope.to.principals.is_empty() {
        roster.active_devices().collect()
    } else {
        for principal in &envelope.to.principals {
            if !roster
                .members()
                .any(|member| member.principal_id == *principal && member.is_active)
            {
                return Err(CoreError::UnknownMember {
                    principal_id: principal.clone(),
                });
            }
        }

        roster
            .active_devices()
            .filter(|device| envelope.to.principals.contains(&device.principal_id))
            .collect()
    };

    if devices.is_empty() {
        return Err(CoreError::NoRecipients);
    }

    Ok(devices)
}

/// Opens a received message, following the validation order of section 18.0.
///
/// `introduced_in_epoch` is the roster epoch established by the publication
/// that introduced this object, which the caller establishes from transport
/// history. It is what makes revocation ordering enforceable: a message is
/// judged by where it actually appeared, not by the epoch it claims.
pub fn open(
    roster: &Roster,
    identity: &DeviceIdentity,
    ciphertext: &[u8],
    introduced_in_epoch: u64,
    now: &str,
) -> Result<QuarantinedMessage> {
    // Steps 1 and 2: the caller has already matched the object against the
    // hash the transport reported. Record the digest so an approval can be
    // bound to it later.
    let ciphertext_sha256 = canonical::sha256_hex(ciphertext);

    // Step 3: decrypt with a local device identity. Size limits are enforced
    // inside, before any expansion.
    let plaintext = identity.decrypt(ciphertext)?;

    // Step 4: parse with duplicate-key rejection.
    let text = std::str::from_utf8(&plaintext).map_err(|_| CoreError::MalformedMessage {
        reason: "message plaintext is not UTF-8".into(),
    })?;
    let signed: SignedObject<MessageEnvelope> = canonical::from_json_str(text)?;

    // Step 5: shape and size validation, before any signature work.
    let envelope = &signed.payload;
    envelope.validate_shape()?;

    if envelope.channel_id != *roster.channel_id() {
        return Err(CoreError::WrongChannel {
            expected: roster.channel_id().to_owned(),
            found: envelope.channel_id.clone(),
        });
    }

    // Step 6: resolve the signer from the roster, never from the message.
    let device =
        roster
            .device(&signed.signer.device_id)
            .ok_or_else(|| CoreError::UnknownDevice {
                device_id: signed.signer.device_id.clone(),
            })?;

    if device.principal_id != signed.signer.principal_id {
        return Err(CoreError::UnauthorizedSigner {
            principal_id: signed.signer.principal_id.clone(),
        });
    }

    // Step 7: verify the signature with the roster's key for that device.
    let key = VerifyingKey::from_base64url("signingKey", &device.signing_key)?;
    key.verify_object(domain::MESSAGE, &signed)?;

    // Step 8: authorization and revocation ordering.
    //
    // Both checks are needed and neither implies the other. The claimed
    // epoch must be one the device was authorized in, and the epoch the
    // object was actually introduced under must be too — otherwise a
    // revoked device could publish a message claiming an epoch from before
    // its revocation (PRD section 17.5, HRC-SEC-013).
    if !device.was_active_in(envelope.roster_epoch) {
        return Err(CoreError::RevokedSigner {
            device_id: device.device_id.clone(),
        });
    }

    if !device.was_active_in(introduced_in_epoch) {
        return Err(CoreError::RevokedSigner {
            device_id: device.device_id.clone(),
        });
    }

    if envelope.roster_epoch > introduced_in_epoch {
        // A message cannot have been encrypted under an epoch that did not
        // exist when it was published.
        return Err(CoreError::EpochOutOfOrder {
            expected: introduced_in_epoch,
            found: envelope.roster_epoch,
        });
    }

    // The addressed device set must match what the roster implies, or the
    // sender's commitment describes a different audience than the channel's
    // membership does (HRC-SEC-016).
    let expected = hrc_protocol::RecipientDevices::new(
        intended_recipients(roster, envelope)?
            .iter()
            .map(|device| device.device_id.clone()),
    )?;
    if expected.devices_hash != envelope.recipients.devices_hash {
        return Err(CoreError::RecipientSetMismatch);
    }

    // This receiver must be among the intended recipients. Decrypting a
    // message that does not name us means the sender's commitment and its
    // encryption disagree.
    let local = identity.recipient().to_string();
    let local_device = roster
        .devices()
        .find(|candidate| candidate.encryption_recipient == local)
        .ok_or(CoreError::LocalDeviceNotInRoster)?;

    if !envelope.recipients.contains(&local_device.device_id) {
        return Err(CoreError::RecipientSetMismatch);
    }

    // Step 9: expiry. Sequence and deduplication are the storage layer's
    // responsibility, since they need state this function does not hold.
    if envelope.is_expired_at(now) {
        return Err(CoreError::MessageExpired {
            expires_at: envelope.expires_at.clone().unwrap_or_default(),
        });
    }

    // Step 10: hand back quarantined content. Nothing here delivers it.
    Ok(QuarantinedMessage {
        sender_principal: device.principal_id.clone(),
        sender_device: device.device_id.clone(),
        envelope: signed.payload,
        ciphertext_sha256,
    })
}

#[cfg(test)]
mod tests;
