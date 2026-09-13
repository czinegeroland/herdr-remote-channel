//! Receipt bodies.
//!
//! PRD section 18.2: a receipt is an ordinary signed and encrypted message
//! whose body names the messages it reports on, the state reached, an
//! optional rejection code, and the receiver's timestamp.
//!
//! Being an ordinary message is the point. A receipt is authenticated and
//! addressed the same way as anything else, so a peer cannot report state on
//! a conversation it is not part of, and a receipt cannot be forged by
//! anyone who could not have sent a message in the first place.

use serde::{Deserialize, Serialize};

use crate::error::{ProtocolError, Result};

/// The states a receipt can report (PRD sections 18.2 and 18.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptState {
    /// The message arrived and was decrypted.
    Delivered,
    /// A human opened it.
    Read,
    /// A human accepted what it asked for.
    Accepted,
    /// It was refused, or could not be handled.
    Rejected,
}

impl ReceiptState {
    /// Every state, in lifecycle order.
    pub const ALL: [ReceiptState; 4] = [
        ReceiptState::Delivered,
        ReceiptState::Read,
        ReceiptState::Accepted,
        ReceiptState::Rejected,
    ];

    /// The wire representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            ReceiptState::Delivered => "delivered",
            ReceiptState::Read => "read",
            ReceiptState::Accepted => "accepted",
            ReceiptState::Rejected => "rejected",
        }
    }

    /// Parses a wire representation, returning `None` for anything else.
    pub fn parse(value: &str) -> Option<ReceiptState> {
        ReceiptState::ALL
            .into_iter()
            .find(|state| state.as_str() == value)
    }

    /// Whether a receipt in this state needs a rejection code.
    ///
    /// A rejection that does not say why leaves the sender unable to act on
    /// it, and a non-rejection carrying a code is contradictory.
    pub const fn requires_rejection_code(self) -> bool {
        matches!(self, ReceiptState::Rejected)
    }
}

/// How many messages one receipt may report on.
///
/// Receipts are batched, but an unbounded list would let one small message
/// make a receiver do arbitrary work looking up identifiers.
pub const MAX_REFERENCED_MESSAGES: usize = 256;

/// The body of a `receipt` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReceiptBody {
    /// The messages this receipt reports on.
    #[serde(rename = "referencedMessageIds")]
    pub referenced_message_ids: Vec<String>,
    /// The state reached.
    pub state: ReceiptState,
    /// Why, when the state is a rejection.
    #[serde(rename = "rejectionCode", skip_serializing_if = "Option::is_none")]
    pub rejection_code: Option<String>,
    /// When the receiver observed it, by the receiver's clock.
    #[serde(rename = "receivedAt")]
    pub received_at: String,
}

impl ReceiptBody {
    /// Builds a receipt for one or more messages.
    pub fn new(
        referenced_message_ids: impl IntoIterator<Item = String>,
        state: ReceiptState,
        received_at: impl Into<String>,
    ) -> Self {
        Self {
            referenced_message_ids: referenced_message_ids.into_iter().collect(),
            state,
            rejection_code: None,
            received_at: received_at.into(),
        }
    }

    /// Attaches a rejection code.
    pub fn with_rejection_code(mut self, code: impl Into<String>) -> Self {
        self.rejection_code = Some(code.into());
        self
    }

    /// Checks the invariants a receiver must not assume.
    pub fn validate(&self) -> Result<()> {
        if self.referenced_message_ids.is_empty() {
            return Err(ProtocolError::MissingField {
                field: "referencedMessageIds",
            });
        }

        if self.referenced_message_ids.len() > MAX_REFERENCED_MESSAGES {
            return Err(ProtocolError::MessageTooLarge {
                size: self.referenced_message_ids.len(),
                limit: MAX_REFERENCED_MESSAGES,
            });
        }

        // Repeats inside one receipt would make a count of reports depend on
        // how the sender chose to batch them.
        let mut seen = std::collections::BTreeSet::new();
        for message_id in &self.referenced_message_ids {
            if message_id.is_empty() {
                return Err(ProtocolError::MissingField {
                    field: "referencedMessageIds",
                });
            }
            if !seen.insert(message_id.as_str()) {
                return Err(ProtocolError::DerivedMismatch {
                    field: "referencedMessageIds",
                });
            }
        }

        if self.received_at.is_empty() {
            return Err(ProtocolError::MissingField {
                field: "receivedAt",
            });
        }

        match (self.state.requires_rejection_code(), &self.rejection_code) {
            (true, None) => Err(ProtocolError::MissingField {
                field: "rejectionCode",
            }),
            (false, Some(_)) => Err(ProtocolError::DerivedMismatch {
                field: "rejectionCode",
            }),
            _ => Ok(()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::canonical;

    fn body() -> ReceiptBody {
        ReceiptBody::new(
            ["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()],
            ReceiptState::Delivered,
            "2026-09-13T00:00:00Z",
        )
    }

    #[test]
    fn a_receipt_round_trips_in_the_documented_shape() {
        let encoded = canonical::to_canonical_json(&body()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();

        assert_eq!(value["state"], "delivered");
        assert!(value["referencedMessageIds"].is_array());
        assert!(
            value.get("rejectionCode").is_none(),
            "an absent code must not serialize as null"
        );

        let decoded: ReceiptBody = canonical::from_json_str(&encoded).unwrap();
        assert_eq!(decoded, body());
    }

    #[test]
    fn every_documented_state_parses() {
        for state in ReceiptState::ALL {
            assert_eq!(ReceiptState::parse(state.as_str()), Some(state));
        }
        assert_eq!(ReceiptState::parse("acknowledged"), None);
    }

    #[test]
    fn a_rejection_must_say_why() {
        let rejection = ReceiptBody::new(
            ["01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned()],
            ReceiptState::Rejected,
            "2026-09-13T00:00:00Z",
        );
        assert!(rejection.validate().is_err());

        rejection
            .with_rejection_code("unsupported_kind")
            .validate()
            .unwrap();
    }

    #[test]
    fn a_non_rejection_carrying_a_code_is_contradictory() {
        assert!(
            body()
                .with_rejection_code("unsupported_kind")
                .validate()
                .is_err()
        );
    }

    #[test]
    fn an_empty_receipt_reports_nothing_and_is_refused() {
        let empty = ReceiptBody::new([], ReceiptState::Delivered, "2026-09-13T00:00:00Z");
        assert!(empty.validate().is_err());
    }

    #[test]
    fn a_receipt_cannot_reference_one_message_twice() {
        // Otherwise a count of reports would depend on how the sender chose
        // to batch them.
        let repeated = ReceiptBody::new(
            ["01ARZ3".to_owned(), "01ARZ3".to_owned()],
            ReceiptState::Delivered,
            "2026-09-13T00:00:00Z",
        );
        assert!(repeated.validate().is_err());
    }

    #[test]
    fn an_unbounded_reference_list_is_refused() {
        let many = ReceiptBody::new(
            (0..MAX_REFERENCED_MESSAGES + 1).map(|index| format!("msg-{index}")),
            ReceiptState::Delivered,
            "2026-09-13T00:00:00Z",
        );
        assert!(matches!(
            many.validate().unwrap_err(),
            ProtocolError::MessageTooLarge { .. }
        ));
    }

    #[test]
    fn a_valid_receipt_passes() {
        body().validate().unwrap();
    }
}
