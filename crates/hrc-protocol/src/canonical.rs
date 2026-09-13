//! RFC 8785 canonical JSON, and the encodings layered on top of it.
//!
//! Signatures are computed over canonical bytes, so two implementations that
//! disagree about canonicalization produce signatures that neither can
//! verify. Everything that turns a protocol value into bytes for signing,
//! hashing, or identifier derivation goes through this module.
//!
//! Canonicalization is delegated to `serde_jcs` rather than written here
//! (PRD decision DEC-016). `tests/rfc8785_vectors.rs` holds the vectors that
//! guard the dependency.

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::error::{ProtocolError, Result};

/// Serializes a value as RFC 8785 canonical JSON.
pub fn to_canonical_json<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    serde_jcs::to_string(value).map_err(ProtocolError::Canonicalization)
}

/// Serializes a value as RFC 8785 canonical JSON bytes.
pub fn to_canonical_bytes<T: Serialize + ?Sized>(value: &T) -> Result<Vec<u8>> {
    to_canonical_json(value).map(String::into_bytes)
}

/// Parses JSON, rejecting objects that repeat a key.
///
/// `serde_json` keeps the last occurrence of a duplicate key by default. That
/// is a signature-verification hazard: a verifier and a consumer can end up
/// reading different values out of the same bytes. PRD section 18.0 makes
/// duplicate-key rejection step 4 of validation, before any field is trusted.
pub fn from_json_str<T: serde::de::DeserializeOwned>(input: &str) -> Result<T> {
    let value: serde_json::Value = serde_json::from_str(input).map_err(ProtocolError::Json)?;
    reject_duplicate_keys(input)?;
    serde_json::from_value(value).map_err(ProtocolError::Json)
}

/// Walks raw JSON text and fails on the first repeated key in any object.
fn reject_duplicate_keys(input: &str) -> Result<()> {
    let mut deserializer = serde_json::Deserializer::from_str(input);
    let scan: DuplicateKeyScan =
        serde::Deserialize::deserialize(&mut deserializer).map_err(ProtocolError::Json)?;

    match scan.0 {
        Some(key) => Err(ProtocolError::DuplicateKey { key }),
        None => Ok(()),
    }
}

/// Scans a JSON document for a repeated object key.
///
/// Deserializing into this type reports the first duplicate key found
/// anywhere in the document, or `None`. It exists because
/// `serde_json::Value` silently keeps the last of a repeated pair, which
/// erases the evidence before anything can reject it.
struct DuplicateKeyScan(Option<String>);

impl<'de> serde::Deserialize<'de> for DuplicateKeyScan {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        deserializer.deserialize_any(DuplicateKeyVisitor)
    }
}

struct DuplicateKeyVisitor;

impl<'de> serde::de::Visitor<'de> for DuplicateKeyVisitor {
    type Value = DuplicateKeyScan;

    fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
        formatter.write_str("any JSON value")
    }

    fn visit_map<A: serde::de::MapAccess<'de>>(
        self,
        mut map: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut seen = std::collections::HashSet::new();
        let mut duplicate = None;

        // Every entry is consumed even after a duplicate is found, because
        // abandoning the map mid-stream would leave the deserializer in an
        // inconsistent state.
        while let Some(key) = map.next_key::<String>()? {
            let repeated = !seen.insert(key.clone());
            let nested = map.next_value::<DuplicateKeyScan>()?;

            if duplicate.is_none() {
                duplicate = if repeated { Some(key) } else { nested.0 };
            }
        }

        Ok(DuplicateKeyScan(duplicate))
    }

    fn visit_seq<A: serde::de::SeqAccess<'de>>(
        self,
        mut seq: A,
    ) -> std::result::Result<Self::Value, A::Error> {
        let mut duplicate = None;
        while let Some(element) = seq.next_element::<DuplicateKeyScan>()? {
            duplicate = duplicate.or(element.0);
        }
        Ok(DuplicateKeyScan(duplicate))
    }

    fn visit_bool<E>(self, _: bool) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
    fn visit_i64<E>(self, _: i64) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
    fn visit_u64<E>(self, _: u64) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
    fn visit_f64<E>(self, _: f64) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
    fn visit_str<E>(self, _: &str) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
    fn visit_unit<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
    fn visit_none<E>(self) -> std::result::Result<Self::Value, E> {
        Ok(DuplicateKeyScan(None))
    }
}

/// Encodes bytes as unpadded base64url, the protocol's binary encoding.
pub fn encode_base64url(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// Decodes an unpadded base64url field.
pub fn decode_base64url(field: &'static str, value: &str) -> Result<Vec<u8>> {
    URL_SAFE_NO_PAD
        .decode(value)
        .map_err(|_| ProtocolError::Base64 { field })
}

/// Returns the lowercase hex SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Returns the lowercase hex SHA-256 digest of a value's canonical encoding.
///
/// This is the derivation used for channel IDs, device IDs, and message chain
/// IDs, all of which the PRD defines as "SHA-256 over the JCS encoding".
pub fn canonical_sha256_hex<T: Serialize + ?Sized>(value: &T) -> Result<String> {
    Ok(sha256_hex(&to_canonical_bytes(value)?))
}

/// Validates that a field holds a lowercase hex digest of the expected size.
pub fn expect_hex_digest(field: &'static str, value: &str, expected_bytes: usize) -> Result<()> {
    let looks_valid = value.len() == expected_bytes * 2
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));

    if looks_valid {
        Ok(())
    } else {
        Err(ProtocolError::Hex {
            field,
            expected_bytes,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn object_keys_are_sorted_and_whitespace_is_removed() {
        let value = serde_json::json!({ "b": 1, "a": 2 });
        assert_eq!(to_canonical_json(&value).unwrap(), r#"{"a":2,"b":1}"#);
    }

    #[test]
    fn duplicate_keys_are_rejected() {
        let error = from_json_str::<serde_json::Value>(r#"{"a":1,"a":2}"#).unwrap_err();
        assert!(
            matches!(&error, ProtocolError::DuplicateKey { key } if key == "a"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duplicate_keys_are_rejected_when_nested() {
        for input in [
            r#"{"outer":{"a":1,"a":2}}"#,
            r#"[{"a":1,"a":2}]"#,
            r#"{"list":[{"ok":1},{"a":1,"a":2}]}"#,
        ] {
            let error = from_json_str::<serde_json::Value>(input).unwrap_err();
            assert!(
                matches!(error, ProtocolError::DuplicateKey { .. }),
                "{input} should be rejected"
            );
        }
    }

    #[test]
    fn distinct_keys_are_accepted() {
        let value: serde_json::Value =
            from_json_str(r#"{"a":1,"b":{"a":2},"c":[{"a":3},{"a":4}]}"#).unwrap();
        assert_eq!(value["b"]["a"], 2);
    }

    #[test]
    fn base64url_round_trips_without_padding() {
        let bytes = [0u8, 1, 2, 250, 251, 252, 253];
        let encoded = encode_base64url(&bytes);
        assert!(!encoded.contains('='), "{encoded} should be unpadded");
        assert_eq!(decode_base64url("test", &encoded).unwrap(), bytes);
    }

    #[test]
    fn base64url_rejects_standard_base64_alphabet() {
        assert!(decode_base64url("test", "++//").is_err());
    }

    #[test]
    fn hex_digests_must_be_lowercase_and_sized() {
        let digest = sha256_hex(b"hrc");
        assert!(expect_hex_digest("test", &digest, 32).is_ok());
        assert!(expect_hex_digest("test", &digest.to_uppercase(), 32).is_err());
        assert!(expect_hex_digest("test", &digest[..62], 32).is_err());
        assert!(expect_hex_digest("test", "", 32).is_err());
    }

    #[test]
    fn canonical_digest_ignores_key_order() {
        let first = serde_json::json!({ "a": 1, "b": [1, 2] });
        let second = serde_json::json!({ "b": [1, 2], "a": 1 });
        assert_eq!(
            canonical_sha256_hex(&first).unwrap(),
            canonical_sha256_hex(&second).unwrap()
        );
    }
}
