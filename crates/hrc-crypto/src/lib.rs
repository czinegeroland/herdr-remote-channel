//! Cryptographic operations for Herdr Remote Channel.
//!
//! Owns Ed25519 signing and verification over the canonical, domain-separated
//! bytes defined by [`hrc_protocol::signed`]. age encryption, invite proofs,
//! and safety-phrase derivation follow in later milestones.
//!
//! Two rules shape this crate, both from PRD section 14:
//!
//! - No custom primitives. Every operation delegates to a maintained crate.
//! - Private keys stay on the device that generated them. Nothing here
//!   returns, prints, serializes, or logs secret material; [`SigningKey`]
//!   has no accessor for its bytes and its `Debug` output is a placeholder.

pub mod encryption;
pub mod enrollment;
pub mod store;

pub use encryption::{DeviceIdentity, DeviceRecipient, encrypt_to};
pub use enrollment::{Invite, SafetyPhrase, invite_proof, safety_phrase, verify_invite_proof};
pub use store::{
    DeviceSecrets, InviteSecret, KeyStore, PassphraseStore, PrincipalSecrets, ProtectedSecret,
};

use ed25519_dalek::{Signer as _, Verifier as _};
use hrc_protocol::canonical;
use hrc_protocol::signed::{SignedObject, Signer};
use serde::Serialize;
use zeroize::Zeroizing;

/// Errors from key handling, signing, and verification.
#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// The operating system refused to provide entropy.
    ///
    /// Generating a key from a degraded random source is worse than
    /// failing, so this is never retried with a fallback.
    #[error("could not read entropy from the operating system: {0}")]
    Entropy(#[source] getrandom::Error),

    /// A public key field was not a valid Ed25519 verifying key.
    #[error("`{field}` is not a valid Ed25519 public key")]
    PublicKey {
        /// The offending field.
        field: &'static str,
    },

    /// A signature field was not 64 bytes of unpadded base64url.
    #[error("`{field}` is not a well-formed Ed25519 signature")]
    SignatureFormat {
        /// The offending field.
        field: &'static str,
    },

    /// The signature did not verify against the key and the signed bytes.
    #[error("signature does not verify for domain `{domain}`")]
    SignatureInvalid {
        /// Domain the verification was attempted under.
        domain: &'static str,
    },

    /// A recipient string was not a valid age X25519 recipient.
    #[error("value is not a valid age X25519 recipient")]
    Recipient,

    /// An identity string was not a valid age X25519 identity.
    #[error("value is not a valid age X25519 identity")]
    Identity,

    /// A message was addressed to no recipients at all.
    #[error("refusing to encrypt to an empty recipient set")]
    NoRecipients,

    /// The ciphertext was malformed, truncated, or altered.
    #[error("ciphertext is malformed or has been altered")]
    Ciphertext,

    /// No local identity could open the ciphertext.
    ///
    /// This is the expected outcome for a device outside the recipient set,
    /// including one revoked before the message was written.
    #[error("no local device identity can decrypt this ciphertext")]
    NotARecipient,

    /// The ciphertext exceeded the size limit before decryption.
    #[error("ciphertext of {size} bytes exceeds the {limit} byte limit")]
    CiphertextTooLarge {
        /// Size of the rejected ciphertext.
        size: usize,
        /// The configured limit.
        limit: usize,
    },

    /// The plaintext exceeded the size limit during decryption.
    #[error("decrypted content exceeds the {limit} byte limit")]
    PlaintextTooLarge {
        /// The configured limit.
        limit: usize,
    },

    /// An invite code was malformed or unsupported.
    #[error("the invite code is malformed or unsupported")]
    MalformedInvite,

    /// An invite secret was below the required strength.
    #[error("invite secret has {bits} bits, below the {minimum_bits} bit minimum")]
    WeakInviteSecret {
        /// Strength of the rejected secret.
        bits: usize,
        /// Minimum the protocol requires.
        minimum_bits: usize,
    },

    /// An invite proof did not verify.
    #[error("the invite proof is not valid for this invite and joiner")]
    InviteProofInvalid,

    /// A key store passphrase was empty.
    #[error("refusing to protect a key with an empty passphrase")]
    EmptyPassphrase,

    /// A key store name was not usable as a file name.
    #[error("`{name}` is not a valid key name")]
    InvalidKeyName {
        /// The rejected name.
        name: String,
    },

    /// No key is stored under that name.
    ///
    /// Distinct from [`CryptoError::KeyStoreUnreadable`]: "not enrolled yet"
    /// and "cannot read your keys" call for very different responses.
    #[error("no key is stored under `{name}`")]
    NoStoredKey {
        /// The name that was looked up.
        name: String,
    },

    /// The stored key could not be decrypted.
    ///
    /// A wrong passphrase and a tampered file are deliberately the same
    /// error: distinguishing them would tell an attacker which of the two
    /// they achieved.
    #[error("the stored key could not be decrypted; the passphrase may be wrong")]
    KeyStoreUnreadable,

    /// The decrypted key document was malformed.
    #[error("the stored key document is corrupt")]
    CorruptKeyStore,

    /// A key store file operation failed.
    #[error("could not {action}: {source}")]
    KeyStoreIo {
        /// What was being attempted.
        action: &'static str,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },

    /// The payload could not be canonicalized.
    #[error(transparent)]
    Protocol(#[from] hrc_protocol::ProtocolError),
}

/// Convenience alias for crypto results.
pub type Result<T> = std::result::Result<T, CryptoError>;

/// Length of an Ed25519 seed, public key, and signature, in bytes.
const SEED_LEN: usize = 32;
const PUBLIC_KEY_LEN: usize = 32;
const SIGNATURE_LEN: usize = 64;

/// An Ed25519 signing key held in memory.
///
/// The inner key zeroizes on drop. There is deliberately no way to read the
/// secret bytes back out: persistence belongs to the keychain-backed store,
/// which is the only component allowed to serialize private material.
pub struct SigningKey(ed25519_dalek::SigningKey);

impl std::fmt::Debug for SigningKey {
    /// Prints the public fingerprint only, never the secret.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SigningKey")
            .field("public", &self.verifying_key().to_base64url())
            .finish_non_exhaustive()
    }
}

impl SigningKey {
    /// Generates a key from operating-system entropy.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SEED_LEN]);
        getrandom::fill(seed.as_mut()).map_err(CryptoError::Entropy)?;
        Ok(Self(ed25519_dalek::SigningKey::from_bytes(&seed)))
    }

    /// Rebuilds a key from a 32-byte seed.
    ///
    /// Only two callers are legitimate: the keychain-backed store loading a
    /// key it previously saved, and tests pinning deterministic vectors.
    pub fn from_seed(seed: &[u8; SEED_LEN]) -> Self {
        Self(ed25519_dalek::SigningKey::from_bytes(seed))
    }

    /// The public half of this key.
    pub fn verifying_key(&self) -> VerifyingKey {
        VerifyingKey(self.0.verifying_key())
    }

    /// Signs the domain-separated canonical encoding of `payload`.
    pub fn sign_payload<P: Serialize + ?Sized>(
        &self,
        domain: &'static str,
        payload: &P,
    ) -> Result<String> {
        let input = hrc_protocol::signature_input(domain, payload)?;
        let signature = self.0.sign(&input);
        Ok(canonical::encode_base64url(&signature.to_bytes()))
    }

    /// Builds a complete signed envelope around `payload`.
    pub fn sign_object<P: Serialize>(
        &self,
        domain: &'static str,
        signer: Signer,
        payload: P,
    ) -> Result<SignedObject<P>> {
        let signature = self.sign_payload(domain, &payload)?;
        Ok(SignedObject {
            version: hrc_protocol::PROTOCOL_VERSION,
            signer,
            payload,
            signature,
        })
    }
}

/// Encodes a seed for storage inside an already-encrypted document.
pub(crate) fn encode_seed(seed: &[u8]) -> String {
    canonical::encode_base64url(seed)
}

/// An Ed25519 public key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyingKey(ed25519_dalek::VerifyingKey);

impl VerifyingKey {
    /// Parses a key from its unpadded base64url wire encoding.
    pub fn from_base64url(field: &'static str, value: &str) -> Result<Self> {
        let bytes = canonical::decode_base64url(field, value)
            .map_err(|_| CryptoError::PublicKey { field })?;
        let bytes: [u8; PUBLIC_KEY_LEN] = bytes
            .try_into()
            .map_err(|_| CryptoError::PublicKey { field })?;

        ed25519_dalek::VerifyingKey::from_bytes(&bytes)
            .map(Self)
            .map_err(|_| CryptoError::PublicKey { field })
    }

    /// The unpadded base64url wire encoding of this key.
    pub fn to_base64url(&self) -> String {
        canonical::encode_base64url(self.0.as_bytes())
    }

    /// Verifies a detached signature over a domain-separated payload.
    pub fn verify_payload<P: Serialize + ?Sized>(
        &self,
        domain: &'static str,
        payload: &P,
        signature: &str,
    ) -> Result<()> {
        let bytes = canonical::decode_base64url("signature", signature)
            .map_err(|_| CryptoError::SignatureFormat { field: "signature" })?;
        let bytes: [u8; SIGNATURE_LEN] = bytes
            .try_into()
            .map_err(|_| CryptoError::SignatureFormat { field: "signature" })?;

        let signature = ed25519_dalek::Signature::from_bytes(&bytes);
        let input = hrc_protocol::signature_input(domain, payload)?;

        self.0
            .verify(&input, &signature)
            .map_err(|_| CryptoError::SignatureInvalid { domain })
    }

    /// Verifies a signed envelope.
    ///
    /// The signature input is rebuilt from the parsed payload, so an envelope
    /// whose stored bytes disagree with its payload fails here rather than
    /// being accepted on the strength of the bytes alone.
    pub fn verify_object<P: Serialize>(
        &self,
        domain: &'static str,
        object: &SignedObject<P>,
    ) -> Result<()> {
        self.verify_payload(domain, &object.payload, &object.signature)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hrc_protocol::domain;
    use serde_json::json;

    fn test_key() -> SigningKey {
        SigningKey::from_seed(&[7u8; SEED_LEN])
    }

    fn signer() -> Signer {
        Signer {
            principal_id: "principal-1".into(),
            device_id: "device-1".into(),
        }
    }

    #[test]
    fn a_signature_verifies_under_its_own_domain() {
        let key = test_key();
        let payload = json!({ "kind": "note" });
        let signature = key.sign_payload(domain::MESSAGE, &payload).unwrap();

        key.verifying_key()
            .verify_payload(domain::MESSAGE, &payload, &signature)
            .unwrap();
    }

    #[test]
    fn a_signature_does_not_verify_under_another_domain() {
        let key = test_key();
        let payload = json!({ "kind": "note" });
        let signature = key.sign_payload(domain::MESSAGE, &payload).unwrap();

        let error = key
            .verifying_key()
            .verify_payload(domain::CONTROL, &payload, &signature)
            .unwrap_err();

        assert!(
            matches!(error, CryptoError::SignatureInvalid { .. }),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_modified_payload_fails_verification() {
        let key = test_key();
        let signature = key
            .sign_payload(domain::MESSAGE, &json!({ "amount": 1 }))
            .unwrap();

        assert!(
            key.verifying_key()
                .verify_payload(domain::MESSAGE, &json!({ "amount": 2 }), &signature)
                .is_err()
        );
    }

    #[test]
    fn reordering_payload_keys_does_not_break_verification() {
        // Canonicalization is what makes this hold: the verifier does not
        // need the sender's field order to reproduce the signed bytes.
        let key = test_key();
        let signature = key
            .sign_payload(domain::MESSAGE, &json!({ "a": 1, "b": 2 }))
            .unwrap();

        key.verifying_key()
            .verify_payload(domain::MESSAGE, &json!({ "b": 2, "a": 1 }), &signature)
            .unwrap();
    }

    #[test]
    fn another_key_cannot_verify_the_signature() {
        let signature = test_key()
            .sign_payload(domain::MESSAGE, &json!({ "kind": "note" }))
            .unwrap();

        let other = SigningKey::from_seed(&[9u8; SEED_LEN]);
        assert!(
            other
                .verifying_key()
                .verify_payload(domain::MESSAGE, &json!({ "kind": "note" }), &signature)
                .is_err()
        );
    }

    #[test]
    fn the_signing_chain_matches_an_independent_implementation() {
        // Cross-implementation vector for seed 0x07 * 32 over
        // `hrc/v1/device-certificate` || 0x00 || `{"deviceId":"d-1"}`.
        // Both values were produced by OpenSSL, not by this crate, so they
        // pin canonicalization, domain separation, Ed25519, and the
        // base64url encoding together. Ed25519 is deterministic, so any
        // drift in any of those layers fails here.
        let key = test_key();

        assert_eq!(
            key.verifying_key().to_base64url(),
            "6kpsY-KcUgq-9VB7Ey7F-ZVHdq6-vnuSQh7qaRRG0iw"
        );
        assert_eq!(
            key.sign_payload(domain::DEVICE_CERTIFICATE, &json!({ "deviceId": "d-1" }))
                .unwrap(),
            "_dmMKdY1QCMQSKQX-55UfVzS3X-YXKjQe5O71xL8KExiA45WdLs6Rva_IoIZ7hSEjYlPfe_2ej6ZVPNu31WyBw"
        );
    }

    #[test]
    fn public_keys_round_trip_through_the_wire_encoding() {
        let key = test_key();
        let encoded = key.verifying_key().to_base64url();
        let parsed = VerifyingKey::from_base64url("signingKey", &encoded).unwrap();
        assert_eq!(parsed, key.verifying_key());
    }

    #[test]
    fn malformed_public_keys_are_rejected() {
        for value in ["", "not base64url!", "c2hvcnQ"] {
            assert!(
                matches!(
                    VerifyingKey::from_base64url("signingKey", value),
                    Err(CryptoError::PublicKey { .. })
                ),
                "{value:?} should be rejected"
            );
        }
    }

    #[test]
    fn malformed_signatures_are_rejected_before_verification() {
        let key = test_key();
        for value in ["", "not base64url!", "c2hvcnQ"] {
            let error = key
                .verifying_key()
                .verify_payload(domain::MESSAGE, &json!({}), value)
                .unwrap_err();
            assert!(
                matches!(error, CryptoError::SignatureFormat { .. }),
                "{value:?} produced {error}"
            );
        }
    }

    #[test]
    fn signed_envelopes_verify_end_to_end() {
        let key = test_key();
        let object = key
            .sign_object(domain::MESSAGE, signer(), json!({ "kind": "note" }))
            .unwrap();

        assert_eq!(object.version, hrc_protocol::PROTOCOL_VERSION);
        key.verifying_key()
            .verify_object(domain::MESSAGE, &object)
            .unwrap();
    }

    #[test]
    fn an_envelope_tampered_after_signing_fails() {
        let key = test_key();
        let mut object = key
            .sign_object(domain::MESSAGE, signer(), json!({ "kind": "note" }))
            .unwrap();

        object.payload = json!({ "kind": "task" });
        assert!(
            key.verifying_key()
                .verify_object(domain::MESSAGE, &object)
                .is_err()
        );
    }

    #[test]
    fn generated_keys_differ() {
        let first = SigningKey::generate().unwrap();
        let second = SigningKey::generate().unwrap();
        assert_ne!(first.verifying_key(), second.verifying_key());
    }

    #[test]
    fn debug_output_never_contains_secret_material() {
        let key = test_key();
        let rendered = format!("{key:?}");

        assert!(rendered.contains(&key.verifying_key().to_base64url()));
        // The seed is all 0x07; its base64url form must not appear anywhere.
        assert!(!rendered.contains(&canonical::encode_base64url(&[7u8; SEED_LEN])));
    }
}
