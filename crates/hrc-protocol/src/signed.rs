//! The signed-object envelope shared by every protocol object.
//!
//! PRD section 18.0 defines one envelope shape and one signature input:
//!
//! ```text
//! ASCII(domain) || 0x00 || UTF8(JCS(payload))
//! ```
//!
//! The domain is what stops a signature made for one object class from
//! verifying as another. A control entry and a message can serialize to
//! identical bytes; only the prefix distinguishes them, so the prefix is
//! built here rather than at each call site.

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::error::Result;

/// Who signed an object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Signer {
    /// Principal the signing device belongs to.
    #[serde(rename = "principalId")]
    pub principal_id: String,
    /// Device whose signing key produced the signature.
    #[serde(rename = "deviceId")]
    pub device_id: String,
}

/// A payload, who signed it, and the signature over it.
///
/// The payload keeps its own type so that a caller works with a typed
/// genesis, control entry, or message rather than untyped JSON. Verification
/// re-derives the signature input from the payload, so a mismatch between the
/// stored bytes and the parsed value cannot slip through.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedObject<P> {
    /// Protocol version of the envelope.
    pub version: u32,
    /// Identity that produced the signature.
    pub signer: Signer,
    /// The signed content.
    pub payload: P,
    /// Unpadded base64url Ed25519 signature over the signature input.
    pub signature: String,
}

impl<P: Serialize> SignedObject<P> {
    /// Rebuilds the exact bytes this object's signature covers.
    pub fn signature_input(&self, domain: &str) -> Result<Vec<u8>> {
        signature_input(domain, &self.payload)
    }
}

/// Builds the signature input for a payload under a domain.
///
/// The `0x00` separator matters: without it, a domain that is a prefix of
/// another domain could be made ambiguous by a crafted payload.
pub fn signature_input<P: Serialize + ?Sized>(domain: &str, payload: &P) -> Result<Vec<u8>> {
    debug_assert!(domain.is_ascii(), "signature domains are ASCII by protocol");

    let canonical = canonical::to_canonical_bytes(payload)?;
    let mut input = Vec::with_capacity(domain.len() + 1 + canonical.len());
    input.extend_from_slice(domain.as_bytes());
    input.push(0x00);
    input.extend_from_slice(&canonical);
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain;

    #[test]
    fn signature_input_is_domain_nul_canonical_payload() {
        let payload = serde_json::json!({ "b": 1, "a": 2 });
        let input = signature_input(domain::MESSAGE, &payload).unwrap();

        let mut expected = b"hrc/v1/message".to_vec();
        expected.push(0x00);
        expected.extend_from_slice(br#"{"a":2,"b":1}"#);
        assert_eq!(input, expected);
    }

    #[test]
    fn the_same_payload_signs_differently_per_domain() {
        let payload = serde_json::json!({ "a": 1 });
        let as_message = signature_input(domain::MESSAGE, &payload).unwrap();
        let as_control = signature_input(domain::CONTROL, &payload).unwrap();
        assert_ne!(as_message, as_control);
    }

    #[test]
    fn the_separator_keeps_prefix_domains_unambiguous() {
        // Without the NUL byte, domain "a" over payload `"bc"` and domain
        // "ab" over payload `"c"` could collide. They must not.
        let first = signature_input("a", &serde_json::json!("bc")).unwrap();
        let second = signature_input("ab", &serde_json::json!("c")).unwrap();
        assert_ne!(first, second);
    }

    #[test]
    fn payload_key_order_does_not_change_the_signature_input() {
        let first: serde_json::Value =
            canonical::from_json_str(r#"{"a":1,"z":{"m":1,"b":2}}"#).unwrap();
        let second: serde_json::Value =
            canonical::from_json_str(r#"{"z":{"b":2,"m":1},"a":1}"#).unwrap();

        assert_eq!(
            signature_input(domain::MESSAGE, &first).unwrap(),
            signature_input(domain::MESSAGE, &second).unwrap()
        );
    }

    #[test]
    fn envelope_round_trips_through_json() {
        let object = SignedObject {
            version: crate::PROTOCOL_VERSION,
            signer: Signer {
                principal_id: "principal".into(),
                device_id: "device".into(),
            },
            payload: serde_json::json!({ "note": "hello" }),
            signature: "c2ln".into(),
        };

        let encoded = canonical::to_canonical_json(&object).unwrap();
        let decoded: SignedObject<serde_json::Value> = canonical::from_json_str(&encoded).unwrap();
        assert_eq!(decoded, object);
    }
}
