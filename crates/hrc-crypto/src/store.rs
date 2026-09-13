//! Where a device's private keys live.
//!
//! PRD section 14.1: private keys are generated on the device, stay there,
//! are stored through the operating-system keychain where available, and are
//! never committed, printed, or transmitted. This module owns that boundary.
//!
//! # The rule that shapes everything here
//!
//! **A missing keychain never silently downgrades to something weaker.**
//!
//! That is the answer to PRD open question OQ-001, recorded as decision
//! DEC-031. A store that quietly wrote plaintext when the platform had no
//! credential service would give the worst possible outcome: the security
//! property is documented, users believe they have it, and nothing tells
//! them they do not. Every backend here either protects the key or refuses
//! to store it.
//!
//! # Backends
//!
//! - [`PassphraseStore`] encrypts the key file with a passphrase, using
//!   age's scrypt recipient. It works everywhere, including headless
//!   servers and CI, and it is fully implemented and tested.
//! - An OS keychain backend is **not implemented yet**. Its place in the
//!   design is the [`KeyStore`] trait; until it exists, HRC-SEC-001 is not
//!   satisfied on platforms where the keychain is the expected home for a
//!   secret.

use std::path::{Path, PathBuf};

use secrecy::{ExposeSecret as _, SecretString};
use zeroize::Zeroizing;

use crate::encryption::DeviceIdentity;
use crate::{CryptoError, Result, SigningKey};

/// Length of an Ed25519 seed.
const SEED_LEN: usize = 32;

/// The private material one device holds for one channel.
///
/// There is no accessor returning the raw seed. A caller gets usable key
/// objects or nothing, so nothing downstream can serialize, print, or
/// transmit the secret by accident.
pub struct DeviceSecrets {
    signing_seed: Zeroizing<[u8; SEED_LEN]>,
    encryption_identity: SecretString,
}

impl std::fmt::Debug for DeviceSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("DeviceSecrets(<redacted>)")
    }
}

impl DeviceSecrets {
    /// Generates fresh device keys from operating-system entropy.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SEED_LEN]);
        getrandom::fill(seed.as_mut()).map_err(CryptoError::Entropy)?;

        let identity = age::x25519::Identity::generate();
        let encryption_identity = age::x25519::Identity::to_string(&identity);

        Ok(Self {
            signing_seed: seed,
            encryption_identity,
        })
    }

    /// The Ed25519 signing key.
    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_seed(&self.signing_seed)
    }

    /// The age decryption identity.
    pub fn device_identity(&self) -> Result<DeviceIdentity> {
        DeviceIdentity::from_secret_string(self.encryption_identity.expose_secret())
    }
}

impl ProtectedSecret for DeviceSecrets {
    fn to_protected_bytes(&self) -> Zeroizing<Vec<u8>> {
        let document = serde_json::json!({
            "version": 1,
            "signingSeed": crate::encode_seed(self.signing_seed.as_slice()),
            "encryptionIdentity": self.encryption_identity.expose_secret(),
        });

        Zeroizing::new(document.to_string().into_bytes())
    }

    fn from_protected_bytes(bytes: &[u8]) -> Result<Self> {
        let document: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| CryptoError::CorruptKeyStore)?;

        let version = document.get("version").and_then(serde_json::Value::as_u64);
        if version != Some(1) {
            return Err(CryptoError::CorruptKeyStore);
        }

        let seed = document
            .get("signingSeed")
            .and_then(serde_json::Value::as_str)
            .ok_or(CryptoError::CorruptKeyStore)?;
        let seed = hrc_protocol::canonical::decode_base64url("signingSeed", seed)
            .map_err(|_| CryptoError::CorruptKeyStore)?;
        let seed: [u8; SEED_LEN] = seed
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::CorruptKeyStore)?;

        let identity = document
            .get("encryptionIdentity")
            .and_then(serde_json::Value::as_str)
            .ok_or(CryptoError::CorruptKeyStore)?;

        Ok(Self {
            signing_seed: Zeroizing::new(seed),
            encryption_identity: SecretString::from(identity.to_owned()),
        })
    }
}

/// A principal's long-term signing identity.
///
/// A principal signs device certificates and join requests. It has no
/// encryption identity, because nothing is ever addressed to a principal —
/// messages go to devices. Keeping the types separate means a principal key
/// cannot be handed to something expecting a device, and no unused age
/// identity is generated and stored for it.
pub struct PrincipalSecrets {
    signing_seed: Zeroizing<[u8; SEED_LEN]>,
}

impl std::fmt::Debug for PrincipalSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PrincipalSecrets(<redacted>)")
    }
}

impl PrincipalSecrets {
    /// Generates a fresh principal key from operating-system entropy.
    pub fn generate() -> Result<Self> {
        let mut seed = Zeroizing::new([0u8; SEED_LEN]);
        getrandom::fill(seed.as_mut()).map_err(CryptoError::Entropy)?;

        Ok(Self { signing_seed: seed })
    }

    /// The Ed25519 signing key.
    pub fn signing_key(&self) -> SigningKey {
        SigningKey::from_seed(&self.signing_seed)
    }
}

/// An invite secret held by the administrator who issued it.
///
/// Retained deliberately. The secret's whole purpose is to be checked later
/// by its issuer: a join request proves possession of it, and verifying that
/// proof needs the same value. What must never happen is publishing it to the
/// channel or emitting it in machine-readable output — not holding it at all.
///
/// It lives in the key store rather than in the database so it gets the same
/// passphrase protection at rest as a signing key, and it is deleted once the
/// invite is spent or withdrawn.
pub struct InviteSecret {
    code: SecretString,
}

impl std::fmt::Debug for InviteSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("InviteSecret(<redacted>)")
    }
}

impl InviteSecret {
    /// Wraps an invite code for storage.
    pub fn new(code: impl Into<String>) -> Self {
        Self {
            code: SecretString::from(code.into()),
        }
    }

    /// The stored invite code.
    pub fn code(&self) -> &str {
        self.code.expose_secret()
    }
}

impl ProtectedSecret for InviteSecret {
    fn to_protected_bytes(&self) -> Zeroizing<Vec<u8>> {
        let document = serde_json::json!({
            "version": 1,
            "kind": "invite",
            "code": self.code.expose_secret(),
        });

        Zeroizing::new(document.to_string().into_bytes())
    }

    fn from_protected_bytes(bytes: &[u8]) -> Result<Self> {
        let document: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| CryptoError::CorruptKeyStore)?;

        if document.get("version").and_then(serde_json::Value::as_u64) != Some(1)
            || document.get("kind").and_then(serde_json::Value::as_str) != Some("invite")
        {
            return Err(CryptoError::CorruptKeyStore);
        }

        let code = document
            .get("code")
            .and_then(serde_json::Value::as_str)
            .ok_or(CryptoError::CorruptKeyStore)?;

        Ok(Self::new(code))
    }
}

/// Something that can be protected at rest by a key store.
///
/// The store encrypts bytes; what those bytes mean is the secret's business.
/// Splitting it this way means device and principal keys share one
/// encrypted-at-rest code path rather than each growing their own.
pub trait ProtectedSecret: Sized {
    /// Serializes to the bytes that get encrypted.
    fn to_protected_bytes(&self) -> Zeroizing<Vec<u8>>;

    /// Parses the bytes recovered from a store.
    fn from_protected_bytes(bytes: &[u8]) -> Result<Self>;
}

impl ProtectedSecret for PrincipalSecrets {
    fn to_protected_bytes(&self) -> Zeroizing<Vec<u8>> {
        let document = serde_json::json!({
            "version": 1,
            "kind": "principal",
            "signingSeed": crate::encode_seed(self.signing_seed.as_slice()),
        });

        Zeroizing::new(document.to_string().into_bytes())
    }

    fn from_protected_bytes(bytes: &[u8]) -> Result<Self> {
        let document: serde_json::Value =
            serde_json::from_slice(bytes).map_err(|_| CryptoError::CorruptKeyStore)?;

        if document.get("version").and_then(serde_json::Value::as_u64) != Some(1) {
            return Err(CryptoError::CorruptKeyStore);
        }

        // A device document parsed as a principal would silently discard the
        // encryption identity, leaving a key that decrypts nothing.
        if document.get("kind").and_then(serde_json::Value::as_str) != Some("principal") {
            return Err(CryptoError::CorruptKeyStore);
        }

        let seed = document
            .get("signingSeed")
            .and_then(serde_json::Value::as_str)
            .ok_or(CryptoError::CorruptKeyStore)?;
        let seed = hrc_protocol::canonical::decode_base64url("signingSeed", seed)
            .map_err(|_| CryptoError::CorruptKeyStore)?;
        let seed: [u8; SEED_LEN] = seed
            .as_slice()
            .try_into()
            .map_err(|_| CryptoError::CorruptKeyStore)?;

        Ok(Self {
            signing_seed: Zeroizing::new(seed),
        })
    }
}

/// Somewhere device keys can be kept.
///
/// Implementations must protect the key at rest or fail. Returning success
/// after writing an unprotected key is not an acceptable implementation of
/// this trait.
pub trait KeyStore {
    /// Persists `secrets` under `name`, replacing anything already there.
    fn save<S: ProtectedSecret>(&self, name: &str, secrets: &S) -> Result<()>;

    /// Loads the secrets stored under `name`.
    fn load<S: ProtectedSecret>(&self, name: &str) -> Result<S>;

    /// Removes the secrets stored under `name`, if any.
    fn delete(&self, name: &str) -> Result<()>;

    /// Whether anything is stored under `name`.
    fn contains(&self, name: &str) -> Result<bool>;
}

/// A key store that encrypts each key file with a passphrase.
///
/// Encryption is age's scrypt recipient, so the key-derivation and cipher
/// choices come from an audited implementation rather than from this
/// project (PRD section 14.2: no custom primitives).
pub struct PassphraseStore {
    directory: PathBuf,
    passphrase: SecretString,
}

impl std::fmt::Debug for PassphraseStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PassphraseStore")
            .field("directory", &self.directory)
            .finish_non_exhaustive()
    }
}

impl PassphraseStore {
    /// Opens a store in `directory`, creating it if necessary.
    pub fn open(directory: impl AsRef<Path>, passphrase: SecretString) -> Result<Self> {
        let directory = directory.as_ref().to_path_buf();

        std::fs::create_dir_all(&directory).map_err(|source| CryptoError::KeyStoreIo {
            action: "create the key store directory",
            source,
        })?;

        restrict_permissions(&directory)?;

        if passphrase.expose_secret().is_empty() {
            // An empty passphrase is indistinguishable from no protection,
            // and would make the file's encryption purely decorative.
            return Err(CryptoError::EmptyPassphrase);
        }

        Ok(Self {
            directory,
            passphrase,
        })
    }

    /// The file backing one stored name.
    fn path_for(&self, name: &str) -> Result<PathBuf> {
        // The name reaches the filesystem, so it may not steer where the
        // write lands.
        let safe = name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
        if !safe || name.is_empty() {
            return Err(CryptoError::InvalidKeyName {
                name: name.to_owned(),
            });
        }

        Ok(self.directory.join(format!("{name}.age")))
    }
}

impl KeyStore for PassphraseStore {
    fn save<S: ProtectedSecret>(&self, name: &str, secrets: &S) -> Result<()> {
        let path = self.path_for(name)?;
        let plaintext = secrets.to_protected_bytes();

        let recipient = age::scrypt::Recipient::new(self.passphrase.clone());
        let ciphertext = crate::encryption::encrypt_to_recipients(
            &[&recipient as &dyn age::Recipient],
            &plaintext,
        )?;

        std::fs::write(&path, &ciphertext).map_err(|source| CryptoError::KeyStoreIo {
            action: "write the key file",
            source,
        })?;
        restrict_permissions(&path)?;

        Ok(())
    }

    fn load<S: ProtectedSecret>(&self, name: &str) -> Result<S> {
        let path = self.path_for(name)?;

        let ciphertext = std::fs::read(&path).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => CryptoError::NoStoredKey {
                name: name.to_owned(),
            },
            _ => CryptoError::KeyStoreIo {
                action: "read the key file",
                source,
            },
        })?;

        let identity = age::scrypt::Identity::new(self.passphrase.clone());
        let plaintext = crate::encryption::decrypt_with_identities(
            &[&identity as &dyn age::Identity],
            &ciphertext,
        )
        .map_err(|_| CryptoError::KeyStoreUnreadable)?;

        S::from_protected_bytes(&plaintext)
    }

    fn delete(&self, name: &str) -> Result<()> {
        let path = self.path_for(name)?;

        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(source) => Err(CryptoError::KeyStoreIo {
                action: "remove the key file",
                source,
            }),
        }
    }

    fn contains(&self, name: &str) -> Result<bool> {
        Ok(self.path_for(name)?.exists())
    }
}

/// Restricts a path to the current user where the platform supports it.
#[cfg(unix)]
fn restrict_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    let metadata = std::fs::metadata(path).map_err(|source| CryptoError::KeyStoreIo {
        action: "read key store permissions",
        source,
    })?;

    let mode = if metadata.is_dir() { 0o700 } else { 0o600 };
    let mut permissions = metadata.permissions();
    permissions.set_mode(mode);

    std::fs::set_permissions(path, permissions).map_err(|source| CryptoError::KeyStoreIo {
        action: "restrict key store permissions",
        source,
    })
}

/// Windows inherits directory ACLs; the passphrase remains the protection.
#[cfg(not(unix))]
fn restrict_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(directory: &tempfile::TempDir) -> PassphraseStore {
        PassphraseStore::open(
            directory.path().join("keys"),
            SecretString::from("correct horse battery staple".to_owned()),
        )
        .unwrap()
    }

    #[test]
    fn keys_round_trip_through_the_store() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);

        let secrets = DeviceSecrets::generate().unwrap();
        let public = secrets.signing_key().verifying_key().to_base64url();
        let recipient = secrets.device_identity().unwrap().recipient().to_string();

        store.save("device-1", &secrets).unwrap();
        let loaded = store.load::<DeviceSecrets>("device-1").unwrap();

        assert_eq!(loaded.signing_key().verifying_key().to_base64url(), public);
        assert_eq!(
            loaded.device_identity().unwrap().recipient().to_string(),
            recipient
        );
    }

    #[test]
    fn a_restored_key_still_signs_and_decrypts() {
        // Round-tripping the public halves is not enough: the restored
        // private material has to actually work.
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);

        let secrets = DeviceSecrets::generate().unwrap();
        store.save("device-1", &secrets).unwrap();
        let loaded = store.load::<DeviceSecrets>("device-1").unwrap();

        let payload = serde_json::json!({ "kind": "note" });
        let signature = loaded
            .signing_key()
            .sign_payload(hrc_protocol::domain::MESSAGE, &payload)
            .unwrap();
        secrets
            .signing_key()
            .verifying_key()
            .verify_payload(hrc_protocol::domain::MESSAGE, &payload, &signature)
            .unwrap();

        let ciphertext = crate::encrypt_to(
            &[secrets.device_identity().unwrap().recipient()],
            b"secret body",
        )
        .unwrap();
        assert_eq!(
            loaded
                .device_identity()
                .unwrap()
                .decrypt(&ciphertext)
                .unwrap()
                .as_slice(),
            b"secret body"
        );
    }

    #[test]
    fn the_key_file_contains_no_plaintext_secret() {
        // The point of the store. If the seed or the age identity appears in
        // the file, the encryption is not doing anything.
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);

        let secrets = DeviceSecrets::generate().unwrap();
        let identity_text = secrets.encryption_identity.expose_secret().to_owned();
        let seed_text = crate::encode_seed(secrets.signing_seed.as_slice());

        store.save("device-1", &secrets).unwrap();

        let bytes = std::fs::read(directory.path().join("keys/device-1.age")).unwrap();
        let text = String::from_utf8_lossy(&bytes);

        assert!(text.starts_with("age-encryption.org/"), "not an age file");
        assert!(!text.contains(&identity_text), "the age identity leaked");
        assert!(!text.contains(&seed_text), "the signing seed leaked");
        assert!(
            !bytes
                .windows(SEED_LEN)
                .any(|window| window == secrets.signing_seed.as_slice()),
            "the raw seed bytes leaked"
        );
    }

    #[test]
    fn a_wrong_passphrase_cannot_read_the_key() {
        let directory = tempfile::tempdir().unwrap();
        store(&directory)
            .save("device-1", &DeviceSecrets::generate().unwrap())
            .unwrap();

        let attacker = PassphraseStore::open(
            directory.path().join("keys"),
            SecretString::from("guess".to_owned()),
        )
        .unwrap();

        assert!(matches!(
            attacker.load::<DeviceSecrets>("device-1").unwrap_err(),
            CryptoError::KeyStoreUnreadable
        ));
    }

    #[test]
    fn a_tampered_key_file_is_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        store
            .save("device-1", &DeviceSecrets::generate().unwrap())
            .unwrap();

        let path = directory.path().join("keys/device-1.age");
        let mut bytes = std::fs::read(&path).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0x01;
        std::fs::write(&path, &bytes).unwrap();

        assert!(matches!(
            store.load::<DeviceSecrets>("device-1").unwrap_err(),
            CryptoError::KeyStoreUnreadable
        ));
    }

    #[test]
    fn an_empty_passphrase_is_refused() {
        // Encrypting with an empty passphrase is decoration, not protection.
        let directory = tempfile::tempdir().unwrap();

        assert!(matches!(
            PassphraseStore::open(
                directory.path().join("keys"),
                SecretString::from(String::new())
            ),
            Err(CryptoError::EmptyPassphrase)
        ));
    }

    #[test]
    fn a_missing_key_is_reported_distinctly() {
        // "Not enrolled yet" and "cannot read your keys" call for very
        // different responses, so they are different errors.
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);

        assert!(matches!(
            store.load::<DeviceSecrets>("device-1").unwrap_err(),
            CryptoError::NoStoredKey { .. }
        ));
        assert!(!store.contains("device-1").unwrap());
    }

    #[test]
    fn key_names_cannot_steer_the_write() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        let secrets = DeviceSecrets::generate().unwrap();

        for name in ["", "../escape", "a/b", "..", "name with spaces", "name.age"] {
            assert!(
                matches!(
                    store.save(name, &secrets),
                    Err(CryptoError::InvalidKeyName { .. })
                ),
                "{name:?} should be refused"
            );
        }
    }

    #[test]
    fn deleting_is_idempotent_and_removes_the_key() {
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);

        store
            .save("device-1", &DeviceSecrets::generate().unwrap())
            .unwrap();
        assert!(store.contains("device-1").unwrap());

        store.delete("device-1").unwrap();
        assert!(!store.contains("device-1").unwrap());

        store
            .delete("device-1")
            .expect("deleting twice is not an error");
    }

    #[test]
    fn a_corrupt_document_inside_a_readable_file_is_rejected() {
        // Decryptable but not parseable: a truncated or downgraded document
        // must not yield half a key.
        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);

        let recipient = age::scrypt::Recipient::new(store.passphrase.clone());
        let ciphertext = crate::encryption::encrypt_to_recipients(
            &[&recipient as &dyn age::Recipient],
            br#"{"version":2,"signingSeed":"AAAA"}"#,
        )
        .unwrap();
        std::fs::write(directory.path().join("keys/device-1.age"), ciphertext).unwrap();

        assert!(matches!(
            store.load::<DeviceSecrets>("device-1").unwrap_err(),
            CryptoError::CorruptKeyStore
        ));
    }

    #[test]
    fn secrets_are_redacted_in_debug_output() {
        let secrets = DeviceSecrets::generate().unwrap();
        let rendered = format!("{secrets:?}");

        assert_eq!(rendered, "DeviceSecrets(<redacted>)");
        assert!(!rendered.contains(secrets.encryption_identity.expose_secret()));
    }

    #[cfg(unix)]
    #[test]
    fn key_files_are_readable_only_by_their_owner() {
        use std::os::unix::fs::PermissionsExt as _;

        let directory = tempfile::tempdir().unwrap();
        let store = store(&directory);
        store
            .save("device-1", &DeviceSecrets::generate().unwrap())
            .unwrap();

        let file = std::fs::metadata(directory.path().join("keys/device-1.age")).unwrap();
        assert_eq!(file.permissions().mode() & 0o777, 0o600);

        let folder = std::fs::metadata(directory.path().join("keys")).unwrap();
        assert_eq!(folder.permissions().mode() & 0o777, 0o700);
    }
}
