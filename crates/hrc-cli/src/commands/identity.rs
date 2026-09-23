//! This installation's identity: initialization and who it is.

use super::*;

/// `hrc init`: generate this device's keys and local state.
pub fn init(context: &Context) -> Result<Value> {
    let paths = &context.paths;
    paths.ensure()?;
    let store = context.key_store()?;

    if store.contains(DEVICE_KEY_NAME)? {
        // Overwriting would destroy the identity this installation is known
        // by, silently orphaning it from every channel it has joined.
        return Err(CliError::AlreadyInitialized);
    }

    let secrets = DeviceSecrets::generate()?;
    let principal = PrincipalSecrets::generate()?;

    // Both keys or neither. An installation with a device key and no
    // principal key could sign messages but could not prove the device was
    // its own, which is a state nothing else knows how to recover from.
    store.save(PRINCIPAL_KEY_NAME, &principal)?;
    store.save(DEVICE_KEY_NAME, &secrets)?;

    // Creating the database after the keys means a half-finished init leaves
    // no state claiming an identity that does not exist.
    Database::open(paths.database())?;

    Ok(json!({
        "status": "ok",
        "home": paths.home().display().to_string(),
        "principalKey": principal.signing_key().verifying_key().to_base64url(),
        "signingKey": secrets.signing_key().verifying_key().to_base64url(),
        "encryptionRecipient": secrets.device_identity()?.recipient().to_string(),
    }))
}

/// This installation's device, as the roster knows it.
pub(super) fn local_device_id(roster: &Roster, device: &DeviceSecrets) -> Result<String> {
    let signing_key = device.signing_key().verifying_key().to_base64url();

    roster
        .devices()
        .find(|candidate| candidate.signing_key == signing_key)
        .map(|candidate| candidate.device_id.clone())
        .ok_or(CliError::LocalDeviceNotInChannel)
}

/// A fresh descriptor nonce.
///
/// Two devices created in the same second with the same keys would otherwise
/// share a descriptor, and therefore a device ID (PRD section 18.0.1).
pub(super) fn random_nonce() -> Result<[u8; 16]> {
    let mut nonce = [0u8; 16];
    getrandom::fill(&mut nonce).map_err(|_| CliError::Entropy)?;
    Ok(nonce)
}

/// `hrc whoami`: report this device's public identity.
/// Who this installation is, as the rest of this crate should read it.
///
/// A principal and a device are different keys. `hrc init` generates both,
/// and the roster lists *principals*. Until `docs/REFACTOR.md` R4 the screens
/// asked `whoami` for "the local principal" and read back `signingKey`, which
/// is the device's key, so no roster entry ever matched: the members screen
/// never recognized this installation, compose offered it as a recipient, and
/// the daemon answered an agent's `whoami` with the device key labelled as the
/// principal. Typed fields are what stop that happening by accident again.
pub struct LocalIdentity {
    /// The principal this installation acts as — what the roster lists.
    pub principal_id: String,
    /// This device's signing key.
    pub device_signing_key: String,
    /// This device's encryption recipient.
    pub encryption_recipient: String,
}

/// Reads this installation's identity from its key store.
pub fn local_identity(context: &Context) -> Result<LocalIdentity> {
    let store = context.key_store()?;
    let device = store.load::<DeviceSecrets>(DEVICE_KEY_NAME)?;
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME)?;

    Ok(LocalIdentity {
        principal_id: principal.signing_key().verifying_key().to_base64url(),
        device_signing_key: device.signing_key().verifying_key().to_base64url(),
        encryption_recipient: device.device_identity()?.recipient().to_string(),
    })
}

/// `hrc whoami`. Its output is unchanged by R4; only what reads it is.
pub fn whoami(context: &Context) -> Result<Value> {
    let identity = local_identity(context)?;

    Ok(json!({
        "status": "ok",
        "signingKey": identity.device_signing_key,
        "encryptionRecipient": identity.encryption_recipient,
    }))
}
