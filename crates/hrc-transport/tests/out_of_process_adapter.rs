//! The JSON-RPC binding must preserve protocol behavior across a process
//! boundary (PRD requirement HRC-TR-001, section 21).
//!
//! The strong claim this file makes is the first test: the *same* conformance
//! suite that validates an in-process adapter passes against one running as a
//! separate executable, spoken to over framed JSON-RPC. A binding that
//! dropped an anomaly, reordered a page, flattened a conflict into a generic
//! failure, or lost a byte in base64 would fail one of those fifteen checks
//! rather than pass quietly.

use std::process::Command;

use hrc_transport::jsonrpc::AdapterProcess;
use hrc_transport::{
    ObjectClass, PublicationClass, PublishObject, PublishRequest, Transport, TransportError,
    conformance,
};

/// Spawns the reference adapter executable this crate builds.
fn adapter() -> AdapterProcess {
    AdapterProcess::spawn(Command::new(env!("CARGO_BIN_EXE_hrc-reference-adapter")))
        .expect("the reference adapter should start and announce itself")
}

#[test]
fn an_out_of_process_adapter_passes_the_whole_conformance_suite() {
    conformance::run_suite(adapter).expect("the out-of-process adapter should conform");
}

#[test]
fn the_handshake_reports_what_the_adapter_guarantees() {
    let adapter = adapter();
    let capabilities = adapter.capabilities();

    assert_eq!(capabilities.protocol, hrc_protocol::TRANSPORT_PROTOCOL_ID);
    assert_eq!(capabilities.history_model, "linear_append_only");
    assert!(capabilities.max_object_bytes > 0);
}

#[test]
fn an_adapter_that_is_not_a_program_is_a_provider_error() {
    // Not a panic and not a hang. A misconfigured adapter path is an
    // ordinary failure the core reports.
    let outcome =
        AdapterProcess::spawn(Command::new("hrc-no-such-adapter-executable-should-exist"));

    match outcome {
        Err(TransportError::Provider(_)) => {}
        Err(other) => panic!("expected a provider error, got {other:?}"),
        Ok(_) => panic!("a missing program should not have started"),
    }
}

#[test]
fn object_bytes_survive_the_boundary_exactly() {
    // Base64 across a JSON boundary is where bytes go to get mangled. The
    // adapter is handed ciphertext and must hand the same bytes back
    // (PRD section 21.3: preserve bytes exactly).
    let mut adapter = adapter();

    let awkward: Vec<u8> = (0u8..=255).collect();
    let genesis = adapter
        .create_group(vec![
            PublishObject {
                name: "protocol.json".into(),
                class: ObjectClass::Protocol,
                bytes: b"{}".to_vec(),
            },
            PublishObject {
                name: "control/000000-genesis.json".into(),
                class: ObjectClass::Control,
                bytes: awkward.clone(),
            },
        ])
        .expect("genesis should publish");

    let record = genesis
        .objects
        .iter()
        .find(|object| object.name == "control/000000-genesis.json")
        .expect("the control object should come back");

    let fetched = adapter
        .get_object(&record.name, &record.sha256)
        .expect("the object should be retrievable");

    assert_eq!(
        fetched, awkward,
        "every byte value must survive the boundary"
    );
}

#[test]
fn a_conflict_crosses_the_boundary_as_a_conflict() {
    // A conflict is the normal outcome of two peers racing, and the caller
    // recovers by rebuilding on the revision it carries. If the binding
    // flattened it into a provider error, the core would stop instead of
    // retrying, and the current revision would be lost.
    let mut adapter = adapter();

    let genesis = adapter
        .create_group(vec![PublishObject {
            name: "protocol.json".into(),
            class: ObjectClass::Protocol,
            bytes: b"{}".to_vec(),
        }])
        .expect("genesis should publish");

    adapter
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![PublishObject {
                name: "messages/2026/09/first.age".into(),
                class: ObjectClass::Message,
                bytes: b"first".to_vec(),
            }],
        })
        .expect("the first publication should succeed");

    // Publishing again against the now-stale genesis revision.
    let error = adapter
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![PublishObject {
                name: "messages/2026/09/second.age".into(),
                class: ObjectClass::Message,
                bytes: b"second".to_vec(),
            }],
        })
        .expect_err("a stale expected revision should conflict");

    match error {
        TransportError::Conflict { current } => {
            let current = current.expect("a conflict must name the revision to rebuild on");
            assert_ne!(current, genesis.revision);
        }
        other => panic!("expected a conflict, got {other:?}"),
    }
}

#[test]
fn a_missing_object_crosses_the_boundary_as_a_missing_object() {
    let adapter = adapter();

    let error = adapter
        .get_object("messages/2026/09/absent.age", &"0".repeat(64))
        .expect_err("an absent object should not be found");

    assert!(
        matches!(error, TransportError::NoSuchObject { .. }),
        "{error:?}"
    );
}

#[test]
fn a_structurally_invalid_publication_is_refused_before_it_is_sent() {
    // The core validates too, rather than relying on the adapter to be the
    // only enforcement of a rule the core depends on.
    let mut adapter = adapter();

    let error = adapter
        .publish(PublishRequest {
            expected_revision: None,
            class: PublicationClass::Control,
            objects: Vec::new(),
        })
        .expect_err("an empty publication is not valid");

    assert!(
        matches!(error, TransportError::InvalidPublication { .. }),
        "{error:?}"
    );
}
