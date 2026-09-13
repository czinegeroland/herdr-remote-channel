//! Joining a channel: the request, the review, and the admission.
//!
//! Enrollment is the only point where someone outside the roster produces an
//! object the channel must consider, so it is the cheapest place to attack
//! membership (PRD section 15). Three operations, deliberately separated:
//!
//! - [`request_join`] builds the joiner's signed, encrypted request.
//! - [`review_join`] runs every check an administrator's *machine* can make
//!   and hands back a [`PendingJoin`] — a proposal, not an admission.
//! - [`admit`] turns an approved proposal into a control entry.
//!
//! The split is the point. Everything a machine can decide happens in
//! `review_join`, and the one thing it cannot — whether the safety phrase the
//! other human read aloud matches — stays a separate, human step. A
//! `PendingJoin` deliberately has no method that admits anyone, so an agent
//! holding one cannot turn it into membership.

use hrc_crypto::enrollment::{Invite, SafetyPhrase, safety_phrase, verify_invite_proof};
use hrc_crypto::{DeviceIdentity, DeviceRecipient, SigningKey, VerifyingKey, encrypt_to};
use hrc_protocol::canonical;
use hrc_protocol::control::{
    ControlEntryPayload, ControlOperation, DeviceCertificate, PrincipalMaterial,
};
use hrc_protocol::domain;
use hrc_protocol::join::{JoinRequestPayload, certificate_hash};
use hrc_protocol::signed::{SignedObject, Signer};

use crate::error::{CoreError, Result};
use crate::roster::{InviteState, Roster};

/// Builds the ciphertext a joiner publishes under `joins/<invite-id>/`.
///
/// The request is signed by the joiner's *principal* key rather than a device
/// key, because the joiner has no device the channel knows yet: the device
/// certificate inside is what introduces one. It is encrypted to the active
/// devices of every administrator, so an ordinary member cannot read pending
/// enrollments.
///
/// The invite proof is computed here from the same certificate that travels
/// in the payload, so the two can never disagree.
pub fn request_join(
    roster: &Roster,
    invite: &Invite,
    principal_signing_key: &SigningKey,
    principal_id: &str,
    device_certificate: DeviceCertificate,
    created_at: &str,
) -> Result<Vec<u8>> {
    invite.validate()?;

    if invite.channel_id != *roster.channel_id() {
        return Err(CoreError::WrongChannel {
            expected: roster.channel_id().to_owned(),
            found: invite.channel_id.clone(),
        });
    }

    if invite.is_expired_at(created_at) {
        return Err(CoreError::InviteExpired {
            invite_id: invite.invite_id.clone(),
            expires_at: invite.expires_at.clone(),
        });
    }

    let principal_public_key = principal_signing_key.verifying_key().to_base64url();
    let certificate_digest = certificate_hash(&device_certificate)?;

    let payload = JoinRequestPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: invite.channel_id.clone(),
        invite_id: invite.invite_id.clone(),
        principal_public_key: principal_public_key.clone(),
        invite_proof: hrc_crypto::enrollment::invite_proof(
            invite,
            &principal_public_key,
            &certificate_digest,
        )?,
        created_at: created_at.to_owned(),
        device_certificate,
    };
    payload.validate_shape()?;

    let signer = Signer {
        principal_id: principal_id.to_owned(),
        device_id: payload.device_certificate.payload.device_id.clone(),
    };
    let signed = principal_signing_key.sign_object(domain::JOIN, signer, payload)?;
    let plaintext = canonical::to_canonical_bytes(&signed)?;

    let recipients = administrator_recipients(roster)?;
    Ok(encrypt_to(&recipients, &plaintext)?)
}

/// The age recipients of every active administrator device.
fn administrator_recipients(roster: &Roster) -> Result<Vec<DeviceRecipient>> {
    let administrators: Vec<&str> = roster
        .members()
        .filter(|member| member.is_administrator && member.is_active)
        .map(|member| member.principal_id.as_str())
        .collect();

    let recipients = roster
        .active_devices()
        .filter(|device| administrators.contains(&device.principal_id.as_str()))
        .map(|device| DeviceRecipient::parse(&device.encryption_recipient))
        .collect::<std::result::Result<Vec<_>, _>>()?;

    if recipients.is_empty() {
        return Err(CoreError::NoRecipients);
    }

    Ok(recipients)
}

/// A join request that passed every machine check, awaiting a human.
///
/// This type carries no method that admits anyone. Producing a control entry
/// takes [`admit`], which an agent-safe surface never calls.
#[derive(Debug, Clone)]
pub struct PendingJoin {
    /// The channel the request is for.
    pub channel_id: String,
    /// The invite it redeems.
    pub invite_id: String,
    /// The principal asking to join.
    pub principal_id: String,
    /// The joiner's principal signing key, as verified against the request.
    pub principal_public_key: String,
    /// The device the joiner introduces.
    pub device_certificate: DeviceCertificate,
    /// When the joiner says they made the request.
    pub created_at: String,
    /// The phrase the two humans compare out of band.
    ///
    /// Displayed, never checked here: an implementation that could confirm a
    /// phrase would be the thing an attacker compromises (PRD section 15.2).
    pub safety_phrase: SafetyPhrase,
}

impl PendingJoin {
    /// The device this join would add.
    pub fn device_id(&self) -> &str {
        &self.device_certificate.payload.device_id
    }
}

/// Validates an encrypted join request and prepares it for human review.
///
/// The order matters, and each step is what makes the next one meaningful:
///
/// 1. Decrypt with a local administrator device identity. Size limits are
///    enforced inside, before any expansion.
/// 2. Parse with duplicate-key rejection, then check the shape. Cheap checks
///    precede every signature verification.
/// 3. Check the channel, and that the invite is one this control log opened,
///    has not been spent or withdrawn, and has not expired. An invite the log
///    does not know about authorizes nothing, whatever proof accompanies it.
/// 4. Verify the device certificate: recompute the device ID from its own
///    descriptor, then check that the *claimed* principal key signed it.
/// 5. Verify the request signature under that same claimed key. Both this
///    and step 4 are self-asserted, which is why neither is sufficient.
/// 6. Verify the invite proof. This is the step that ties the self-asserted
///    material to an authorization the administrator actually issued, and it
///    commits to the principal key and the certificate checked above — so a
///    request cannot carry a proof made for a different joiner.
/// 7. Refuse a principal that is already a member.
///
/// Only then is a safety phrase derived, because deriving one for material
/// that failed a check would put an authentic-looking phrase in front of a
/// human.
pub fn review_join(
    roster: &Roster,
    invite: &Invite,
    identity: &DeviceIdentity,
    ciphertext: &[u8],
    now: &str,
) -> Result<PendingJoin> {
    let administrator = roster
        .members()
        .find(|member| member.is_administrator && member.is_active)
        .ok_or(CoreError::NotAnAdministrator)?;

    let plaintext = identity.decrypt(ciphertext)?;
    let text = std::str::from_utf8(&plaintext).map_err(|_| CoreError::MalformedJoinRequest {
        reason: "join request plaintext is not UTF-8".into(),
    })?;
    let signed: SignedObject<JoinRequestPayload> = canonical::from_json_str(text)?;

    let request = &signed.payload;
    request.validate_shape()?;

    if request.channel_id != *roster.channel_id() {
        return Err(CoreError::WrongChannel {
            expected: roster.channel_id().to_owned(),
            found: request.channel_id.clone(),
        });
    }

    // The invite the caller holds and the invite the request names must be
    // the same one, or the proof below would be checked against a secret the
    // request was never built for.
    if request.invite_id != invite.invite_id || invite.channel_id != request.channel_id {
        return Err(CoreError::MalformedJoinRequest {
            reason: "the request names a different invite".into(),
        });
    }

    match roster.invite(&request.invite_id) {
        Some(InviteState::Open { expires_at }) => {
            if now >= expires_at.as_str() || invite.is_expired_at(now) {
                return Err(CoreError::InviteExpired {
                    invite_id: request.invite_id.clone(),
                    expires_at: expires_at.clone(),
                });
            }
        }
        Some(InviteState::Consumed { .. }) => {
            return Err(CoreError::InviteAlreadyUsed {
                invite_id: request.invite_id.clone(),
            });
        }
        Some(InviteState::Revoked) => {
            return Err(CoreError::InviteRevoked {
                invite_id: request.invite_id.clone(),
            });
        }
        None => {
            return Err(CoreError::UnknownInvite {
                invite_id: request.invite_id.clone(),
            });
        }
    }

    let certificate = &request.device_certificate;
    certificate.payload.verify_device_id()?;

    if certificate.payload.descriptor.principal_id != signed.signer.principal_id {
        return Err(CoreError::CertificatePrincipalMismatch {
            expected: signed.signer.principal_id.clone(),
            found: certificate.payload.descriptor.principal_id.clone(),
        });
    }

    if signed.signer.device_id != certificate.payload.device_id {
        return Err(CoreError::MalformedJoinRequest {
            reason: "the request names a device other than the one it certifies".into(),
        });
    }

    let claimed_key =
        VerifyingKey::from_base64url("principalPublicKey", &request.principal_public_key)?;
    claimed_key.verify_object(domain::DEVICE_CERTIFICATE, certificate)?;
    claimed_key.verify_object(domain::JOIN, &signed)?;

    let certificate_digest = certificate_hash(certificate)?;
    verify_invite_proof(
        invite,
        &request.principal_public_key,
        &certificate_digest,
        &request.invite_proof,
    )?;

    if roster
        .members()
        .any(|member| member.principal_id == signed.signer.principal_id && member.is_active)
    {
        return Err(CoreError::AlreadyEnrolled {
            principal_id: signed.signer.principal_id.clone(),
        });
    }

    let phrase = safety_phrase(
        &request.channel_id,
        &request.invite_id,
        &administrator.principal_signing_key,
        &request.principal_public_key,
        &certificate_digest,
    )?;

    Ok(PendingJoin {
        channel_id: request.channel_id.clone(),
        invite_id: request.invite_id.clone(),
        principal_id: signed.signer.principal_id.clone(),
        principal_public_key: request.principal_public_key.clone(),
        device_certificate: certificate.clone(),
        created_at: request.created_at.clone(),
        safety_phrase: phrase,
    })
}

/// The phrase a joiner displays while waiting, from public material only.
///
/// The joiner cannot see the administrator's copy, so both sides derive it
/// independently and a substituted identity on either side produces a
/// different phrase.
pub fn joiner_safety_phrase(
    roster: &Roster,
    invite: &Invite,
    principal_public_key: &str,
    device_certificate: &DeviceCertificate,
) -> Result<SafetyPhrase> {
    let administrator = roster
        .members()
        .find(|member| member.is_administrator && member.is_active)
        .ok_or(CoreError::NotAnAdministrator)?;

    Ok(safety_phrase(
        roster.channel_id(),
        &invite.invite_id,
        &administrator.principal_signing_key,
        principal_public_key,
        &certificate_hash(device_certificate)?,
    )?)
}

/// Turns an approved [`PendingJoin`] into a signed control entry.
///
/// Calling this *is* the approval: there is no separate flag to set, so an
/// agent cannot approve a join by supplying the right argument to something
/// it was already allowed to call. The trusted human interface is the only
/// caller (PRD section 24.2).
///
/// The entry names the invite it spends, which is what lets every other
/// participant reject a second admission under the same authorization rather
/// than taking the administrator's word for it.
pub fn admit(
    roster: &Roster,
    signing_key: &SigningKey,
    signer: Signer,
    pending: &PendingJoin,
    created_at: &str,
) -> Result<SignedObject<ControlEntryPayload>> {
    if pending.channel_id != *roster.channel_id() {
        return Err(CoreError::WrongChannel {
            expected: roster.channel_id().to_owned(),
            found: pending.channel_id.clone(),
        });
    }

    let member = roster
        .members()
        .find(|member| member.principal_id == signer.principal_id)
        .ok_or_else(|| CoreError::UnknownMember {
            principal_id: signer.principal_id.clone(),
        })?;

    if !member.is_administrator || !member.is_active {
        return Err(CoreError::UnauthorizedSigner {
            principal_id: signer.principal_id.clone(),
        });
    }

    let operation = ControlOperation::AddMember {
        invite_id: pending.invite_id.clone(),
        member: PrincipalMaterial {
            principal_id: pending.principal_id.clone(),
            principal_signing_key: pending.principal_public_key.clone(),
            devices: vec![pending.device_certificate.clone()],
        },
    };

    let payload = ControlEntryPayload {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: roster.channel_id().to_owned(),
        sequence: roster.sequence() + 1,
        previous_hash: roster.head_hash().to_owned(),
        epoch: roster.epoch() + 1,
        created_at: created_at.to_owned(),
        operation,
    };
    payload.validate_shape()?;

    Ok(signing_key.sign_object(domain::CONTROL, signer, payload)?)
}

#[cfg(test)]
mod tests;
