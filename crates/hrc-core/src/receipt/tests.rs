//! Receipt and correlation tests.
//!
//! A receipt is a claim about someone else's message, which is exactly the
//! kind of statement worth forging. These check that a peer can only report
//! on messages it was actually addressed on, and that an answer can only
//! attach itself to a question that was really asked.

use super::*;

use hrc_protocol::canonical;
use hrc_protocol::message::{Addressing, MessageEnvelope};

const NOW: &str = "2026-09-13T00:00:00Z";
const MESSAGE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
const THREAD: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

/// The device that was legitimately addressed.
fn recipient_device() -> String {
    canonical::sha256_hex(b"bob-device")
}

/// A quarantined receipt message from `bob`.
fn receipt_from(device: &str) -> QuarantinedMessage {
    let envelope = MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: canonical::sha256_hex(b"channel"),
        roster_epoch: 1,
        message_id: "01BX5ZZKBKACTAV9WEVGEMMVRZ".into(),
        device_sequence: 1,
        previous_chain_id: None,
        created_at: NOW.into(),
        expires_at: None,
        to: Addressing {
            principals: vec!["alice".into()],
            endpoint: None,
        },
        recipients: hrc_protocol::RecipientDevices::new([canonical::sha256_hex(b"alice-device")])
            .unwrap(),
        recipient_previous_chain_ids: None,
        thread_id: THREAD.into(),
        in_reply_to: None,
        kind: "receipt".into(),
        requested_capability: None,
        body: serde_json::json!({}),
        attachments: Vec::new(),
        padding: String::new(),
    };

    QuarantinedMessage {
        envelope,
        sender_principal: "bob".into(),
        sender_device: device.to_owned(),
        ciphertext_sha256: canonical::sha256_hex(b"ciphertext"),
    }
}

/// What this installation sent, as its outbox records it.
fn sent() -> Vec<SentMessage> {
    vec![SentMessage {
        message_id: MESSAGE.into(),
        recipient_device_ids: vec![recipient_device()],
        thread_id: THREAD.into(),
        kind: "question".into(),
    }]
}

#[test]
fn a_receipt_from_an_addressed_device_is_accepted() {
    let receipt = receipt_from(&recipient_device());
    let body = build_receipt([MESSAGE.to_owned()], ReceiptState::Delivered, NOW).unwrap();

    let verified = accept_receipt(&receipt, &body, &sent()).unwrap();

    assert_eq!(verified.len(), 1);
    assert_eq!(verified[0].message_id, MESSAGE);
    assert_eq!(verified[0].reporter_principal, "bob");
    assert_eq!(verified[0].state, ReceiptState::Delivered);
    assert_eq!(verified[0].reported_at, NOW);
}

#[test]
fn a_receipt_from_a_device_that_was_never_addressed_is_refused() {
    // The forgery this check exists for: a channel member reporting on a
    // message that was not sent to it.
    let outsider = canonical::sha256_hex(b"carol-device");
    let receipt = receipt_from(&outsider);
    let body = build_receipt([MESSAGE.to_owned()], ReceiptState::Delivered, NOW).unwrap();

    let error = accept_receipt(&receipt, &body, &sent()).unwrap_err();

    assert!(matches!(error, CoreError::ReceiptFromNonRecipient { .. }));
}

#[test]
fn a_receipt_about_a_message_we_did_not_send_is_refused() {
    let receipt = receipt_from(&recipient_device());
    let body = build_receipt(
        ["01BX5ZZKBKACTAV9WEVGEMMVAA".to_owned()],
        ReceiptState::Delivered,
        NOW,
    )
    .unwrap();

    assert!(matches!(
        accept_receipt(&receipt, &body, &sent()).unwrap_err(),
        CoreError::ReceiptForUnknownMessage { .. }
    ));
}

#[test]
fn one_bad_reference_refuses_the_whole_batch() {
    // Receipts are batched. Accepting the good half would record a report
    // from a peer that demonstrably lied in the same breath.
    let receipt = receipt_from(&recipient_device());
    let body = build_receipt(
        [MESSAGE.to_owned(), "01BX5ZZKBKACTAV9WEVGEMMVAA".to_owned()],
        ReceiptState::Delivered,
        NOW,
    )
    .unwrap();

    assert!(accept_receipt(&receipt, &body, &sent()).is_err());
}

#[test]
fn every_state_survives_verification() {
    let receipt = receipt_from(&recipient_device());

    for state in ReceiptState::ALL {
        let body = if state.requires_rejection_code() {
            build_rejection([MESSAGE.to_owned()], "unsupported_kind", NOW).unwrap()
        } else {
            build_receipt([MESSAGE.to_owned()], state, NOW).unwrap()
        };

        let verified = accept_receipt(&receipt, &body, &sent()).unwrap();
        assert_eq!(verified[0].state, state);
    }
}

#[test]
fn a_rejection_carries_its_code_through() {
    let receipt = receipt_from(&recipient_device());
    let body = build_rejection([MESSAGE.to_owned()], "unsupported_kind", NOW).unwrap();

    let verified = accept_receipt(&receipt, &body, &sent()).unwrap();
    assert_eq!(
        verified[0].rejection_code.as_deref(),
        Some("unsupported_kind")
    );
}

#[test]
fn a_malformed_receipt_body_is_refused_before_anything_is_looked_up() {
    let receipt = receipt_from(&recipient_device());
    let empty = ReceiptBody::new([], ReceiptState::Delivered, NOW);

    assert!(accept_receipt(&receipt, &empty, &sent()).is_err());
}

// --- Answer correlation ---

/// An answer envelope naming `in_reply_to` in `thread`.
fn answer(in_reply_to: Option<&str>, thread: &str, kind: &str) -> MessageEnvelope {
    let mut envelope = receipt_from(&recipient_device()).envelope;
    envelope.kind = kind.to_owned();
    envelope.thread_id = thread.to_owned();
    envelope.in_reply_to = in_reply_to.map(str::to_owned);
    envelope
}

#[test]
fn an_answer_correlates_to_the_question_it_names() {
    let correlated = correlate_answer(&answer(Some(MESSAGE), THREAD, "answer"), &sent()).unwrap();
    assert_eq!(correlated, MESSAGE);
}

#[test]
fn an_answer_naming_nothing_does_not_correlate() {
    assert!(matches!(
        correlate_answer(&answer(None, THREAD, "answer"), &sent()).unwrap_err(),
        CoreError::UncorrelatedAnswer { .. }
    ));
}

#[test]
fn an_answer_to_a_message_we_never_sent_does_not_correlate() {
    assert!(
        correlate_answer(
            &answer(Some("01BX5ZZKBKACTAV9WEVGEMMVAA"), THREAD, "answer"),
            &sent()
        )
        .is_err()
    );
}

#[test]
fn an_answer_to_something_that_is_not_a_question_does_not_correlate() {
    let mut sent = sent();
    sent[0].kind = "note".into();

    assert!(correlate_answer(&answer(Some(MESSAGE), THREAD, "answer"), &sent).is_err());
}

#[test]
fn an_answer_cannot_move_a_question_into_another_thread() {
    // Otherwise a peer could answer in whichever conversation suited it.
    let elsewhere = "01BX5ZZKBKACTAV9WEVGEMMVRZ";
    assert!(correlate_answer(&answer(Some(MESSAGE), elsewhere, "answer"), &sent()).is_err());
}

#[test]
fn correlation_refuses_a_message_that_is_not_an_answer() {
    assert!(matches!(
        correlate_answer(&answer(Some(MESSAGE), THREAD, "note"), &sent()).unwrap_err(),
        CoreError::NotAnAnswer { .. }
    ));
}
