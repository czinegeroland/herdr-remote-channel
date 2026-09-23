//! Wire protocol constants and vocabulary for Herdr Remote Channel.
//!
//! This crate owns the values that both sides of a channel must agree on
//! byte for byte: the protocol version, the domain separation strings used
//! as signature prefixes, and the closed set of message kinds.
//!
//! See PRD sections 18.0 and 18.2.

pub mod attachment;
pub mod canonical;
pub mod control;
pub mod delegation;
pub mod error;
pub mod identity;
pub mod join;
pub mod message;
pub mod receipt;
pub mod recipients;
pub mod signed;
pub mod ulid;

pub use attachment::{Attachment, safe_file_name, validate_attachments};
pub use control::{ControlEntryPayload, ControlOperation, GenesisPayload};
pub use delegation::{DelegationState, ProgressBody, Reference, ResultBody, TaskBody};
pub use error::{ProtocolError, Result};
pub use identity::{DeviceCertificatePayload, DeviceDescriptor};
pub use message::{Addressing, MessageEnvelope};
pub use receipt::{ReceiptBody, ReceiptState};
pub use recipients::RecipientDevices;
pub use signed::{SignedObject, Signer, signature_input};
pub use ulid::{is_ulid, ulid_at};

/// Version carried by every signed protocol object.
pub const PROTOCOL_VERSION: u32 = 1;

/// Identifier of the transport adapter JSON-RPC protocol (PRD section 21).
pub const TRANSPORT_PROTOCOL_ID: &str = "hrc.transport/1";

/// Domain separation prefixes for Ed25519 signatures (PRD section 18.0).
///
/// The signature input is `ASCII(domain) || 0x00 || UTF8(JCS(payload))`, so
/// these strings are protocol constants and may never be changed silently.
pub mod domain {
    /// Genesis control object establishing channel identity.
    pub const GENESIS: &str = "hrc/v1/genesis";
    /// Append-only membership and policy control log entry.
    pub const CONTROL: &str = "hrc/v1/control";
    /// Device certificate signed by a principal key.
    pub const DEVICE_CERTIFICATE: &str = "hrc/v1/device-certificate";
    /// Join request published by an enrolling peer.
    pub const JOIN: &str = "hrc/v1/join";
    /// Message envelope.
    pub const MESSAGE: &str = "hrc/v1/message";
    /// Context package manifest.
    pub const CONTEXT: &str = "hrc/v1/context";
    /// HMAC domain for the one-time invite proof.
    pub const INVITE_PROOF: &str = "hrc/v1/invite-proof";
    /// Derivation domain for the enrollment safety phrase.
    pub const SAFETY_PHRASE: &str = "hrc/v1/safety-phrase";
    /// Derivation domain for the per-device message chain ID.
    pub const MESSAGE_CHAIN: &str = "hrc/v1/message-chain";
}

/// The closed set of message kinds defined by PRD section 18.2.
///
/// A kind that does not parse is stored as unsupported and answered with a
/// rejection receipt. It never triggers execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MessageKind {
    /// Informational message.
    Note,
    /// Request for an answer.
    Question,
    /// Response to a question.
    Answer,
    /// Structured delegation request. Communication only; never executed.
    Task,
    /// Receiver accepts responsibility for a task.
    TaskAccept,
    /// Receiver declines a task.
    TaskDecline,
    /// Coarse task update.
    Progress,
    /// Human-reviewed task result.
    Result,
    /// Best-effort withdrawal.
    Cancel,
    /// Delivery, read, acceptance, or rejection state.
    Receipt,
    /// Protocol and limit negotiation.
    Capabilities,
}

impl MessageKind {
    /// Every kind, in the order documented by the PRD.
    pub const ALL: [MessageKind; 11] = [
        MessageKind::Note,
        MessageKind::Question,
        MessageKind::Answer,
        MessageKind::Task,
        MessageKind::TaskAccept,
        MessageKind::TaskDecline,
        MessageKind::Progress,
        MessageKind::Result,
        MessageKind::Cancel,
        MessageKind::Receipt,
        MessageKind::Capabilities,
    ];

    /// The wire representation of this kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            MessageKind::Note => "note",
            MessageKind::Question => "question",
            MessageKind::Answer => "answer",
            MessageKind::Task => "task",
            MessageKind::TaskAccept => "task_accept",
            MessageKind::TaskDecline => "task_decline",
            MessageKind::Progress => "progress",
            MessageKind::Result => "result",
            MessageKind::Cancel => "cancel",
            MessageKind::Receipt => "receipt",
            MessageKind::Capabilities => "capabilities",
        }
    }

    /// Parses a wire representation, returning `None` for unknown kinds.
    pub fn parse(value: &str) -> Option<MessageKind> {
        MessageKind::ALL
            .into_iter()
            .find(|kind| kind.as_str() == value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn message_kinds_round_trip() {
        for kind in MessageKind::ALL {
            assert_eq!(MessageKind::parse(kind.as_str()), Some(kind));
        }
    }

    #[test]
    fn unknown_message_kinds_do_not_parse() {
        for value in ["", "NOTE", "execute", "shell", "note "] {
            assert_eq!(
                MessageKind::parse(value),
                None,
                "unexpectedly parsed {value:?}"
            );
        }
    }

    #[test]
    fn wire_names_are_unique() {
        let mut names: Vec<&str> = MessageKind::ALL.iter().map(|kind| kind.as_str()).collect();
        names.sort_unstable();
        let count = names.len();
        names.dedup();
        assert_eq!(names.len(), count);
    }

    #[test]
    fn signature_domains_are_version_pinned() {
        for value in [
            domain::GENESIS,
            domain::CONTROL,
            domain::DEVICE_CERTIFICATE,
            domain::JOIN,
            domain::MESSAGE,
            domain::CONTEXT,
            domain::INVITE_PROOF,
            domain::SAFETY_PHRASE,
            domain::MESSAGE_CHAIN,
        ] {
            assert!(
                value.starts_with("hrc/v1/"),
                "{value} is not version pinned"
            );
            assert!(value.is_ascii(), "{value} is not ASCII");
        }
    }
}
