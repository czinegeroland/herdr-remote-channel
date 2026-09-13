//! age encryption to an explicit set of recipient devices.
//!
//! PRD section 14.2 fixes the format: age with X25519 recipients. Section
//! 14.3 fixes the order: a message is signed first, then the signed object is
//! encrypted. Signing after encrypting would authenticate ciphertext rather
//! than content, so nothing here accepts an unsigned payload — callers pass
//! already-serialized signed bytes.
//!
//! Recipients are always passed explicitly. There is no ambient "current
//! channel roster" here, because deciding who may read a message is a
//! membership question that belongs to the core, not to the cipher layer.

use std::io::{Read as _, Write as _};
use std::str::FromStr as _;

use zeroize::Zeroizing;

use crate::{CryptoError, Result};

/// Largest ciphertext this build will decrypt, in bytes.
///
/// PRD section 20.3 caps a single attachment ciphertext at 25 MiB, and
/// section 25 (HRC-SEC-006, HRC-SEC-007) requires a size check *before*
/// decryption so that a hostile object cannot expand into memory first.
pub const MAX_CIPHERTEXT_BYTES: usize = 25 * 1024 * 1024;

/// Largest plaintext this build will produce from a decryption, in bytes.
///
/// age is not a compressed format, but this bound is enforced anyway so that
/// a future format change cannot turn decryption into an unbounded write.
pub const MAX_PLAINTEXT_BYTES: usize = 25 * 1024 * 1024;

/// An X25519 decryption identity belonging to one device.
///
/// As with the signing key, there is no accessor for the secret and `Debug`
/// prints only the public recipient.
pub struct DeviceIdentity(age::x25519::Identity);

impl std::fmt::Debug for DeviceIdentity {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceIdentity")
            .field("recipient", &self.recipient().to_string())
            .finish_non_exhaustive()
    }
}

impl DeviceIdentity {
    /// Generates an identity from operating-system entropy.
    pub fn generate() -> Self {
        Self(age::x25519::Identity::generate())
    }

    /// Parses an `AGE-SECRET-KEY-1...` identity.
    ///
    /// Only the keychain-backed store and tests should call this.
    pub fn from_secret_string(value: &str) -> Result<Self> {
        age::x25519::Identity::from_str(value)
            .map(Self)
            .map_err(|_| CryptoError::Identity)
    }

    /// The public recipient other devices encrypt to.
    pub fn recipient(&self) -> DeviceRecipient {
        DeviceRecipient(self.0.to_public())
    }

    /// Decrypts a ciphertext addressed to this identity.
    pub fn decrypt(&self, ciphertext: &[u8]) -> Result<Zeroizing<Vec<u8>>> {
        if ciphertext.len() > MAX_CIPHERTEXT_BYTES {
            return Err(CryptoError::CiphertextTooLarge {
                size: ciphertext.len(),
                limit: MAX_CIPHERTEXT_BYTES,
            });
        }

        let decryptor = age::Decryptor::new(ciphertext).map_err(|_| CryptoError::Ciphertext)?;
        let reader = decryptor
            .decrypt([&self.0 as &dyn age::Identity].into_iter())
            .map_err(|_| CryptoError::NotARecipient)?;

        // Read one byte past the limit so an oversized plaintext is detected
        // rather than silently truncated to the cap.
        let mut plaintext = Zeroizing::new(Vec::new());
        reader
            .take(MAX_PLAINTEXT_BYTES as u64 + 1)
            .read_to_end(&mut plaintext)
            .map_err(|_| CryptoError::Ciphertext)?;

        if plaintext.len() > MAX_PLAINTEXT_BYTES {
            return Err(CryptoError::PlaintextTooLarge {
                limit: MAX_PLAINTEXT_BYTES,
            });
        }

        Ok(plaintext)
    }
}

/// An X25519 recipient: the public half of a device's encryption key.
#[derive(Debug, Clone)]
pub struct DeviceRecipient(age::x25519::Recipient);

impl DeviceRecipient {
    /// Parses an `age1...` recipient from a device descriptor.
    pub fn parse(value: &str) -> Result<Self> {
        age::x25519::Recipient::from_str(value)
            .map(Self)
            .map_err(|_| CryptoError::Recipient)
    }
}

impl std::fmt::Display for DeviceRecipient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}", self.0)
    }
}

/// Encrypts `plaintext` to every recipient in `recipients`.
///
/// The recipient list must be non-empty: age would otherwise produce a
/// well-formed file that nobody can open, which is worse than an error
/// because it looks like a delivered message.
pub fn encrypt_to(recipients: &[DeviceRecipient], plaintext: &[u8]) -> Result<Vec<u8>> {
    if recipients.is_empty() {
        return Err(CryptoError::NoRecipients);
    }

    let boxed: Vec<&dyn age::Recipient> = recipients
        .iter()
        .map(|recipient| &recipient.0 as &dyn age::Recipient)
        .collect();

    let encryptor =
        age::Encryptor::with_recipients(boxed.into_iter()).map_err(|_| CryptoError::Recipient)?;

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|_| CryptoError::Ciphertext)?;
    writer
        .write_all(plaintext)
        .map_err(|_| CryptoError::Ciphertext)?;
    writer.finish().map_err(|_| CryptoError::Ciphertext)?;

    Ok(ciphertext)
}

/// Encrypts to arbitrary age recipients.
///
/// [`encrypt_to`] is the channel-facing entry point and takes device
/// recipients only; the key store needs a passphrase recipient, which is the
/// same age operation with a different recipient type.
pub(crate) fn encrypt_to_recipients(
    recipients: &[&dyn age::Recipient],
    plaintext: &[u8],
) -> Result<Vec<u8>> {
    if recipients.is_empty() {
        return Err(CryptoError::NoRecipients);
    }

    let encryptor = age::Encryptor::with_recipients(recipients.iter().copied())
        .map_err(|_| CryptoError::Recipient)?;

    let mut ciphertext = Vec::new();
    let mut writer = encryptor
        .wrap_output(&mut ciphertext)
        .map_err(|_| CryptoError::Ciphertext)?;
    writer
        .write_all(plaintext)
        .map_err(|_| CryptoError::Ciphertext)?;
    writer.finish().map_err(|_| CryptoError::Ciphertext)?;

    Ok(ciphertext)
}

/// Decrypts with arbitrary age identities, under the same size limits.
pub(crate) fn decrypt_with_identities(
    identities: &[&dyn age::Identity],
    ciphertext: &[u8],
) -> Result<Zeroizing<Vec<u8>>> {
    if ciphertext.len() > MAX_CIPHERTEXT_BYTES {
        return Err(CryptoError::CiphertextTooLarge {
            size: ciphertext.len(),
            limit: MAX_CIPHERTEXT_BYTES,
        });
    }

    let decryptor = age::Decryptor::new(ciphertext).map_err(|_| CryptoError::Ciphertext)?;
    let reader = decryptor
        .decrypt(identities.iter().copied())
        .map_err(|_| CryptoError::NotARecipient)?;

    let mut plaintext = Zeroizing::new(Vec::new());
    reader
        .take(MAX_PLAINTEXT_BYTES as u64 + 1)
        .read_to_end(&mut plaintext)
        .map_err(|_| CryptoError::Ciphertext)?;

    if plaintext.len() > MAX_PLAINTEXT_BYTES {
        return Err(CryptoError::PlaintextTooLarge {
            limit: MAX_PLAINTEXT_BYTES,
        });
    }

    Ok(plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// age hands back its secret encoding wrapped in `secrecy::SecretString`.
    use secrecy::ExposeSecret as _;

    #[test]
    fn a_recipient_can_read_what_was_addressed_to_it() {
        let identity = DeviceIdentity::generate();
        let ciphertext = encrypt_to(&[identity.recipient()], b"staging is failing").unwrap();

        assert_eq!(
            identity.decrypt(&ciphertext).unwrap().as_slice(),
            b"staging is failing"
        );
    }

    #[test]
    fn every_listed_recipient_can_read_the_same_ciphertext() {
        // One message, one ciphertext object, several devices: this is what
        // lets a channel broadcast without re-encrypting per device.
        let devices: Vec<DeviceIdentity> = (0..3).map(|_| DeviceIdentity::generate()).collect();
        let recipients: Vec<DeviceRecipient> =
            devices.iter().map(DeviceIdentity::recipient).collect();

        let ciphertext = encrypt_to(&recipients, b"shared body").unwrap();

        for device in &devices {
            assert_eq!(
                device.decrypt(&ciphertext).unwrap().as_slice(),
                b"shared body"
            );
        }
    }

    #[test]
    fn a_device_outside_the_recipient_set_cannot_read_it() {
        let addressed = DeviceIdentity::generate();
        let outsider = DeviceIdentity::generate();

        let ciphertext = encrypt_to(&[addressed.recipient()], b"private").unwrap();

        assert!(matches!(
            outsider.decrypt(&ciphertext).unwrap_err(),
            CryptoError::NotARecipient
        ));
    }

    #[test]
    fn a_revoked_device_left_out_of_the_set_cannot_read_later_messages() {
        // The shape of revocation: the removed device keeps its key, but is
        // simply not among the recipients of anything encrypted afterwards.
        let staying = DeviceIdentity::generate();
        let revoked = DeviceIdentity::generate();

        let before = encrypt_to(&[staying.recipient(), revoked.recipient()], b"before").unwrap();
        let after = encrypt_to(&[staying.recipient()], b"after").unwrap();

        assert_eq!(revoked.decrypt(&before).unwrap().as_slice(), b"before");
        assert!(revoked.decrypt(&after).is_err());
        assert_eq!(staying.decrypt(&after).unwrap().as_slice(), b"after");
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let identity = DeviceIdentity::generate();
        let mut ciphertext = encrypt_to(&[identity.recipient()], b"authentic").unwrap();

        // Flip a bit in the payload region rather than the header.
        let last = ciphertext.len() - 1;
        ciphertext[last] ^= 0x01;

        assert!(identity.decrypt(&ciphertext).is_err());
    }

    #[test]
    fn truncated_ciphertext_is_rejected() {
        let identity = DeviceIdentity::generate();
        let ciphertext = encrypt_to(&[identity.recipient()], b"authentic").unwrap();

        assert!(
            identity
                .decrypt(&ciphertext[..ciphertext.len() / 2])
                .is_err()
        );
    }

    #[test]
    fn garbage_is_rejected_without_panicking() {
        let identity = DeviceIdentity::generate();
        for input in [b"".as_slice(), b"not age", &[0xff; 64]] {
            assert!(identity.decrypt(input).is_err());
        }
    }

    #[test]
    fn oversized_ciphertext_is_refused_before_decryption() {
        let identity = DeviceIdentity::generate();
        let oversized = vec![0u8; MAX_CIPHERTEXT_BYTES + 1];

        assert!(matches!(
            identity.decrypt(&oversized).unwrap_err(),
            CryptoError::CiphertextTooLarge { .. }
        ));
    }

    #[test]
    fn encrypting_to_nobody_is_an_error() {
        assert!(matches!(
            encrypt_to(&[], b"body").unwrap_err(),
            CryptoError::NoRecipients
        ));
    }

    #[test]
    fn recipients_round_trip_through_their_wire_encoding() {
        let identity = DeviceIdentity::generate();
        let encoded = identity.recipient().to_string();

        assert!(encoded.starts_with("age1"));
        let parsed = DeviceRecipient::parse(&encoded).unwrap();
        assert_eq!(parsed.to_string(), encoded);

        let ciphertext = encrypt_to(&[parsed], b"via wire form").unwrap();
        assert_eq!(
            identity.decrypt(&ciphertext).unwrap().as_slice(),
            b"via wire form"
        );
    }

    #[test]
    fn malformed_recipients_are_rejected() {
        for value in ["", "age1", "not-a-recipient", "AGE-SECRET-KEY-1"] {
            assert!(
                matches!(DeviceRecipient::parse(value), Err(CryptoError::Recipient)),
                "{value:?} should be rejected"
            );
        }
    }

    #[test]
    fn identities_round_trip_through_their_secret_encoding() {
        let identity = DeviceIdentity::generate();
        let secret = age::x25519::Identity::to_string(&identity.0);

        let restored = DeviceIdentity::from_secret_string(secret.expose_secret()).unwrap();
        assert_eq!(
            restored.recipient().to_string(),
            identity.recipient().to_string()
        );
    }

    #[test]
    fn malformed_identities_are_rejected() {
        assert!(matches!(
            DeviceIdentity::from_secret_string("not-a-key"),
            Err(CryptoError::Identity)
        ));
    }

    #[test]
    fn debug_output_never_contains_secret_material() {
        let identity = DeviceIdentity::generate();
        let secret = age::x25519::Identity::to_string(&identity.0);
        let rendered = format!("{identity:?}");

        assert!(rendered.contains(&identity.recipient().to_string()));
        assert!(!rendered.contains(secret.expose_secret()));
    }
}
