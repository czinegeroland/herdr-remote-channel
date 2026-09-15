//! The message envelope.
//!
//! PRD section 18.1 defines one envelope for every message kind. It is the
//! `payload` of an `hrc/v1/message` signed object, and that signed object is
//! then encrypted to the intended recipient devices — signed first, then
//! encrypted, so the signature authenticates content rather than ciphertext
//! (section 14.3).
//!
//! Everything a receiver needs in order to decide whether to accept a message
//! is inside the signed payload: the channel, the roster epoch, the sender,
//! the position in that sender's chain, and the exact device set the sender
//! addressed. Git paths and commit metadata are never trusted as message
//! metadata (section 18.0).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::canonical;
use crate::error::{ProtocolError, Result};
use crate::recipients::RecipientDevices;

/// Largest plaintext message this build will accept, in bytes.
///
/// PRD section 20.3: 1 MiB is the hard maximum for message plaintext.
pub const MAX_PLAINTEXT_BYTES: usize = 1024 * 1024;

/// Padding buckets, in bytes.
///
/// PRD section 14.3 step 5 requires the plaintext to be padded into a size
/// bucket before encryption. Without it, ciphertext length leaks message
/// length to anyone who can see the repository — which, for a public
/// channel, is everyone.
const PADDING_BUCKETS: [usize; 6] = [1024, 4096, 16_384, 65_536, 262_144, MAX_PLAINTEXT_BYTES];

/// The last message chain link observed by the sender for each recipient device.
///
/// The map is optional for wire compatibility with envelopes produced before
/// recipient-scoped ordering was introduced. A present map is complete: its
/// keys must exactly equal `recipientDeviceIds`, including a `null` value for
/// a device that has no predecessor yet.
pub type RecipientPredecessors = BTreeMap<String, Option<String>>;

/// Where a message is addressed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Addressing {
    /// Recipient principals. Empty means a channel broadcast.
    #[serde(default)]
    pub principals: Vec<String>,
    /// Optional logical endpoint, for example `reviewer`.
    ///
    /// Advisory only. A sender cannot target a local pane, and the receiver
    /// decides whether anything reaches an agent (PRD section 11.6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// Validates a logical endpoint identifier.
///
/// PRD section 19.1 allows an endpoint to appear on the agent-safe surface
/// before approval, but only when it matches this closed pattern. Anything
/// else is sender-controlled free text and stays quarantined with the body.
pub fn is_valid_endpoint(value: &str) -> bool {
    let mut characters = value.chars();

    match characters.next() {
        Some(first) if first.is_ascii_lowercase() => {}
        _ => return false,
    }

    value.len() <= 32
        && characters.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-')
}

/// The signed payload of a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageEnvelope {
    /// Protocol version.
    pub version: u32,
    /// Channel this message belongs to.
    #[serde(rename = "channelId")]
    pub channel_id: String,
    /// Roster epoch the message was encrypted under.
    #[serde(rename = "rosterEpoch")]
    pub roster_epoch: u64,
    /// Sortable message identifier.
    #[serde(rename = "messageId")]
    pub message_id: String,
    /// Per-device sequence number.
    #[serde(rename = "deviceSequence")]
    pub device_sequence: u64,
    /// Chain ID of the preceding message from this device.
    #[serde(rename = "previousChainId")]
    pub previous_chain_id: Option<String>,
    /// RFC 3339 UTC creation time.
    #[serde(rename = "createdAt")]
    pub created_at: String,
    /// RFC 3339 UTC expiry, when the message expires.
    #[serde(rename = "expiresAt")]
    pub expires_at: Option<String>,
    /// Where the message is addressed.
    pub to: Addressing,
    /// The device set the sender encrypted to, and its commitment.
    #[serde(flatten)]
    pub recipients: RecipientDevices,
    /// Per-recipient predecessor links for recipient-scoped ordering.
    #[serde(
        rename = "recipientPreviousChainIds",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    pub recipient_previous_chain_ids: Option<RecipientPredecessors>,
    /// Thread this message belongs to.
    #[serde(rename = "threadId")]
    pub thread_id: String,
    /// Message being replied to, if any.
    #[serde(rename = "inReplyTo")]
    pub in_reply_to: Option<String>,
    /// Message kind.
    pub kind: String,
    /// Capability the sender is requesting, for example `prompt:request`.
    #[serde(
        rename = "requestedCapability",
        skip_serializing_if = "Option::is_none"
    )]
    pub requested_capability: Option<String>,
    /// Kind-specific content.
    pub body: serde_json::Value,
    /// Attachment references.
    ///
    /// References rather than content: the declared size travels in the
    /// signed envelope, so a receiver can refuse an attachment before
    /// fetching or decrypting anything (PRD requirement HRC-SEC-006).
    #[serde(default)]
    pub attachments: Vec<crate::attachment::Attachment>,
    /// Padding to a size bucket. Carries no meaning.
    pub padding: String,
}

impl MessageEnvelope {
    /// Checks the invariants that need no roster or local state.
    ///
    /// This is validation step 5 of PRD section 18.0, and it runs before any
    /// signature work: rejecting a malformed envelope is cheap, and
    /// verifying a signature over nonsense proves only that the nonsense was
    /// signed.
    pub fn validate_shape(&self) -> Result<()> {
        if self.version != crate::PROTOCOL_VERSION {
            return Err(ProtocolError::UnsupportedVersion {
                found: self.version,
                expected: crate::PROTOCOL_VERSION,
            });
        }

        canonical::expect_hex_digest("channelId", &self.channel_id, 32)?;
        self.recipients.verify()?;
        if let Some(predecessors) = &self.recipient_previous_chain_ids {
            let keys = predecessors.keys().cloned().collect::<Vec<_>>();
            if keys != self.recipients.device_ids {
                return Err(ProtocolError::DerivedMismatch {
                    field: "recipientPreviousChainIds",
                });
            }

            for (device_id, predecessor) in predecessors {
                canonical::expect_hex_digest("recipientPreviousChainIds", device_id, 32)?;
                if let Some(predecessor) = predecessor {
                    canonical::expect_hex_digest("recipientPreviousChainIds", predecessor, 32)?;
                }
            }
        }
        crate::attachment::validate_attachments(&self.attachments)?;

        if self.message_id.is_empty() {
            return Err(ProtocolError::DerivedMismatch { field: "messageId" });
        }

        if self.thread_id.is_empty() {
            return Err(ProtocolError::DerivedMismatch { field: "threadId" });
        }

        if self.device_sequence == 0 {
            // Sequences start at one, so zero means the sender never
            // allocated through the transactional path.
            return Err(ProtocolError::InvalidControlSequence { sequence: 0 });
        }

        if let Some(endpoint) = &self.to.endpoint
            && !is_valid_endpoint(endpoint)
        {
            return Err(ProtocolError::InvalidEndpoint {
                endpoint: endpoint.clone(),
            });
        }

        Ok(())
    }

    /// Whether the message has expired at `now`.
    ///
    /// Comparison is lexicographic over RFC 3339 UTC timestamps, which is
    /// correct for the `Z`-suffixed, fixed-width form the protocol uses and
    /// avoids a date-library dependency in the protocol crate.
    pub fn is_expired_at(&self, now: &str) -> bool {
        self.expires_at
            .as_deref()
            .is_some_and(|expiry| expiry <= now)
    }

    /// Returns this recipient's predecessor when recipient ordering is present.
    ///
    /// The outer option distinguishes a legacy envelope with no recipient
    /// ordering map from a new envelope whose entry is explicitly `null`.
    pub fn recipient_predecessor(&self, device_id: &str) -> Option<Option<&str>> {
        self.recipient_previous_chain_ids
            .as_ref()
            .map(|predecessors| predecessors.get(device_id).and_then(Option::as_deref))
    }
}

/// Pads `plaintext` up to the next bucket boundary.
///
/// The padding goes in the envelope's `padding` field before serialization,
/// so a caller pads the logical message rather than the ciphertext.
pub fn padding_for(serialized_len: usize) -> Result<String> {
    let target = PADDING_BUCKETS
        .iter()
        .copied()
        .find(|bucket| *bucket >= serialized_len)
        .ok_or(ProtocolError::MessageTooLarge {
            size: serialized_len,
            limit: MAX_PLAINTEXT_BYTES,
        })?;

    // A single repeated character canonicalizes and compresses predictably;
    // the field exists to occupy space, not to carry entropy.
    Ok("0".repeat(target.saturating_sub(serialized_len)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn device(seed: &str) -> String {
        canonical::sha256_hex(seed.as_bytes())
    }

    fn envelope() -> MessageEnvelope {
        MessageEnvelope {
            version: crate::PROTOCOL_VERSION,
            channel_id: canonical::sha256_hex(b"channel"),
            roster_epoch: 1,
            message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            device_sequence: 1,
            previous_chain_id: None,
            created_at: "2026-09-13T00:00:00Z".into(),
            expires_at: None,
            to: Addressing {
                principals: vec!["principal-2".into()],
                endpoint: None,
            },
            recipients: RecipientDevices::new([device("a"), device("b")]).unwrap(),
            recipient_previous_chain_ids: None,
            thread_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            in_reply_to: None,
            kind: "note".into(),
            requested_capability: None,
            body: serde_json::json!({ "text": "staging is failing" }),
            attachments: Vec::new(),
            padding: String::new(),
        }
    }

    #[test]
    fn a_well_formed_envelope_validates() {
        envelope().validate_shape().unwrap();
    }

    #[test]
    fn the_envelope_round_trips_in_the_specified_shape() {
        let encoded = canonical::to_canonical_json(&envelope()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();

        // The recipient commitment is flattened into the envelope, as
        // PRD section 18.1 shows, not nested under a sub-object.
        assert!(value["recipientDeviceIds"].is_array());
        assert!(value["recipientDevicesHash"].is_string());
        assert!(value.get("recipientPreviousChainIds").is_none());
        assert_eq!(value["kind"], "note");

        let decoded: MessageEnvelope = canonical::from_json_str(&encoded).unwrap();
        assert_eq!(decoded, envelope());
    }

    #[test]
    fn an_unsupported_version_is_rejected() {
        let mut message = envelope();
        message.version = 99;

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::UnsupportedVersion { found: 99, .. }
        ));
    }

    #[test]
    fn a_tampered_recipient_commitment_is_rejected() {
        let mut message = envelope();
        message.recipients.device_ids.push(device("z"));
        message.recipients.device_ids.sort();

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::DerivedMismatch { .. }
        ));
    }

    #[test]
    fn recipient_predecessors_round_trip_and_match_the_recipient_set() {
        let mut message = envelope();
        let previous = canonical::sha256_hex(b"previous");
        message.recipient_previous_chain_ids = Some(BTreeMap::from([
            (device("a"), None),
            (device("b"), Some(previous.clone())),
        ]));

        message.validate_shape().unwrap();
        assert_eq!(message.recipient_predecessor(&device("a")), Some(None));
        assert_eq!(
            message.recipient_predecessor(&device("b")),
            Some(Some(previous.as_str()))
        );

        let encoded = canonical::to_canonical_json(&message).unwrap();
        let decoded: MessageEnvelope = canonical::from_json_str(&encoded).unwrap();
        assert_eq!(decoded, message);
    }

    #[test]
    fn recipient_predecessors_must_name_every_and_only_recipient() {
        let mut message = envelope();
        message.recipient_previous_chain_ids = Some(BTreeMap::from([(device("a"), None)]));

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::DerivedMismatch {
                field: "recipientPreviousChainIds"
            }
        ));
    }

    #[test]
    fn recipient_predecessor_links_are_digests() {
        let mut message = envelope();
        message.recipient_previous_chain_ids = Some(BTreeMap::from([
            (device("a"), None),
            (device("b"), Some("not-a-chain-id".into())),
        ]));

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::Hex {
                field: "recipientPreviousChainIds",
                ..
            }
        ));
    }

    #[test]
    fn a_zero_device_sequence_is_rejected() {
        let mut message = envelope();
        message.device_sequence = 0;

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::InvalidControlSequence { sequence: 0 }
        ));
    }

    #[test]
    fn valid_endpoints_are_accepted() {
        for value in ["reviewer", "a", "code-review", "agent_2", &"a".repeat(32)] {
            assert!(is_valid_endpoint(value), "{value} should be valid");
        }
    }

    #[test]
    fn invalid_endpoints_are_rejected() {
        // These are the shapes that must never reach an agent-safe surface
        // as-is: uppercase, leading digits, spaces, markup, and overlong
        // values are all sender-controlled text pretending to be an
        // identifier (PRD section 19.1).
        for value in [
            "",
            "Reviewer",
            "1reviewer",
            "-reviewer",
            "review er",
            "review<script>",
            "réviewer",
            &"a".repeat(33),
        ] {
            assert!(!is_valid_endpoint(value), "{value:?} should be rejected");
        }
    }

    #[test]
    fn an_invalid_endpoint_fails_envelope_validation() {
        let mut message = envelope();
        message.to.endpoint = Some("Not An Endpoint".into());

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::InvalidEndpoint { .. }
        ));
    }

    #[test]
    fn expiry_is_compared_against_the_supplied_time() {
        let mut message = envelope();
        assert!(
            !message.is_expired_at("2026-09-13T00:00:00Z"),
            "no expiry set"
        );

        message.expires_at = Some("2026-09-20T00:00:00Z".into());
        assert!(!message.is_expired_at("2026-09-19T23:59:59Z"));
        assert!(message.is_expired_at("2026-09-20T00:00:00Z"));
        assert!(message.is_expired_at("2026-09-21T00:00:00Z"));
    }

    #[test]
    fn padding_rounds_up_to_a_bucket() {
        assert_eq!(padding_for(0).unwrap().len(), 1024);
        assert_eq!(padding_for(1024).unwrap().len(), 0);
        assert_eq!(padding_for(1025).unwrap().len(), 4096 - 1025);
        assert_eq!(padding_for(70_000).unwrap().len(), 262_144 - 70_000);
    }

    #[test]
    fn padding_hides_small_length_differences() {
        // Two messages of different lengths in the same bucket must pad to
        // the same total, or the padding would not hide anything.
        let short = 100 + padding_for(100).unwrap().len();
        let longer = 900 + padding_for(900).unwrap().len();
        assert_eq!(short, longer);
    }

    #[test]
    fn a_message_beyond_the_hard_maximum_is_refused() {
        assert!(matches!(
            padding_for(MAX_PLAINTEXT_BYTES + 1).unwrap_err(),
            ProtocolError::MessageTooLarge { .. }
        ));
    }

    #[test]
    fn an_envelope_rejects_an_attachment_set_over_the_limit() {
        // The check belongs on the envelope rather than at each call site,
        // so a message carrying too much cannot be validated by a caller
        // that forgot to look at its attachments.
        use crate::attachment::Attachment;

        let mut message = envelope();
        message.attachments = (0..6)
            .map(|index| Attachment {
                name: format!("blob-{index}"),
                media_type: "application/octet-stream".into(),
                ciphertext_sha256: canonical::sha256_hex(&[index as u8]),
                ciphertext_bytes: 5 * 1024 * 1024,
                plaintext_bytes: 1024,
            })
            .collect();

        assert!(matches!(
            message.validate_shape().unwrap_err(),
            ProtocolError::MessageTooLarge { .. }
        ));
    }

    #[test]
    fn an_envelope_accepts_attachments_within_the_limit() {
        use crate::attachment::Attachment;

        let mut message = envelope();
        message.attachments = vec![Attachment {
            name: "findings.txt".into(),
            media_type: "text/plain".into(),
            ciphertext_sha256: canonical::sha256_hex(b"blob"),
            ciphertext_bytes: 4096,
            plaintext_bytes: 4032,
        }];

        message.validate_shape().unwrap();
    }
}
