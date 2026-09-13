//! Receipts and question-answer correlation.
//!
//! A receipt tells a sender what happened to something they sent. That makes
//! it worth forging: a peer that could report on messages it never received
//! could tell a sender their message landed when it did not, or claim a
//! rejection that never happened.
//!
//! Two checks close that. A receipt is an ordinary signed and encrypted
//! message, so the reporter is resolved from the roster before its body is
//! trusted at all — that comes free from [`crate::message::open`]. On top of
//! that, [`accept_receipt`] requires the reporting device to have been an
//! intended recipient of each message it reports on, which the sender knows
//! because the sender chose that recipient set and committed to it.
//!
//! Correlation is the same idea applied to answers: an answer is only an
//! answer to a question the receiver actually asked, in the thread it was
//! asked in.

use hrc_protocol::message::MessageEnvelope;
use hrc_protocol::receipt::{ReceiptBody, ReceiptState};

use crate::error::{CoreError, Result};
use crate::message::QuarantinedMessage;

/// What a sender knows about a message it published.
///
/// Supplied by the caller from local outbox state rather than taken from the
/// receipt, since a receipt asserting its own validity would assert nothing.
#[derive(Debug, Clone)]
pub struct SentMessage {
    /// The message identifier.
    pub message_id: String,
    /// The devices it was encrypted to.
    pub recipient_device_ids: Vec<String>,
    /// The thread it belongs to.
    pub thread_id: String,
    /// Its kind.
    pub kind: String,
}

/// One verified report about one message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifiedReceipt {
    /// The message reported on.
    pub message_id: String,
    /// The verified reporting principal.
    pub reporter_principal: String,
    /// The verified reporting device.
    pub reporter_device: String,
    /// The state reported.
    pub state: ReceiptState,
    /// Why, when the state is a rejection.
    pub rejection_code: Option<String>,
    /// The reporter's timestamp, which is theirs and not ours.
    pub reported_at: String,
}

/// Builds the body of a receipt for messages that just arrived.
pub fn build_receipt(
    referenced: impl IntoIterator<Item = String>,
    state: ReceiptState,
    received_at: &str,
) -> Result<ReceiptBody> {
    let body = ReceiptBody::new(referenced, state, received_at);
    body.validate()?;
    Ok(body)
}

/// Builds the body of a rejection receipt.
///
/// PRD section 18.2 requires one for a message kind this build does not
/// understand. Answering with silence would leave the sender unable to tell
/// an unsupported kind from a peer that is offline.
pub fn build_rejection(
    referenced: impl IntoIterator<Item = String>,
    code: &str,
    received_at: &str,
) -> Result<ReceiptBody> {
    let body =
        ReceiptBody::new(referenced, ReceiptState::Rejected, received_at).with_rejection_code(code);
    body.validate()?;
    Ok(body)
}

/// Verifies a receipt against what the sender actually sent.
///
/// Every referenced message must be one this installation sent, and the
/// reporting device must have been among its intended recipients. A report
/// about a message the reporter was never addressed on is refused outright
/// rather than recorded and discounted later: a stored claim tends to be
/// read as a fact.
pub fn accept_receipt(
    receipt: &QuarantinedMessage,
    body: &ReceiptBody,
    sent: &[SentMessage],
) -> Result<Vec<VerifiedReceipt>> {
    body.validate()?;

    let mut verified = Vec::with_capacity(body.referenced_message_ids.len());

    for message_id in &body.referenced_message_ids {
        let original = sent
            .iter()
            .find(|candidate| candidate.message_id == *message_id)
            .ok_or_else(|| CoreError::ReceiptForUnknownMessage {
                message_id: message_id.clone(),
            })?;

        if !original
            .recipient_device_ids
            .contains(&receipt.sender_device)
        {
            return Err(CoreError::ReceiptFromNonRecipient {
                message_id: message_id.clone(),
                reporter_device: receipt.sender_device.clone(),
            });
        }

        verified.push(VerifiedReceipt {
            message_id: message_id.clone(),
            reporter_principal: receipt.sender_principal.clone(),
            reporter_device: receipt.sender_device.clone(),
            state: body.state,
            rejection_code: body.rejection_code.clone(),
            reported_at: body.received_at.clone(),
        });
    }

    Ok(verified)
}

/// Checks that an answer answers a question this installation asked.
///
/// An answer whose `inReplyTo` names something else — a note, a message in
/// another thread, or nothing this installation sent — is still a message,
/// and it is still delivered. What it is not is *an answer*, and presenting
/// it as one would let a peer attach a reply to whichever of your questions
/// it preferred.
pub fn correlate_answer(answer: &MessageEnvelope, sent: &[SentMessage]) -> Result<String> {
    if answer.kind != hrc_protocol::MessageKind::Answer.as_str() {
        return Err(CoreError::NotAnAnswer {
            kind: answer.kind.clone(),
        });
    }

    let in_reply_to = answer
        .in_reply_to
        .as_ref()
        .ok_or(CoreError::UncorrelatedAnswer {
            reason: "the answer names no question",
        })?;

    let question = sent
        .iter()
        .find(|candidate| candidate.message_id == *in_reply_to)
        .ok_or(CoreError::UncorrelatedAnswer {
            reason: "the named question was not sent from this installation",
        })?;

    if question.kind != hrc_protocol::MessageKind::Question.as_str() {
        return Err(CoreError::UncorrelatedAnswer {
            reason: "the named message is not a question",
        });
    }

    if question.thread_id != answer.thread_id {
        // Otherwise a peer could answer in whichever thread suited it, and
        // the answer would read as belonging to a conversation it is not in.
        return Err(CoreError::UncorrelatedAnswer {
            reason: "the answer is in a different thread from its question",
        });
    }

    Ok(question.message_id.clone())
}

#[cfg(test)]
mod tests;
