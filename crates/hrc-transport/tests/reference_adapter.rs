//! The in-memory reference adapter must pass the published conformance suite.
//!
//! PRD requirement HRC-TR-007. This test is what makes the suite meaningful:
//! without an adapter that is not Git passing it, the contract would only
//! ever have been exercised by the implementation it was written around.

use hrc_transport::memory::MemoryTransport;
use hrc_transport::{
    ObjectClass, PublicationClass, PublishObject, PublishRequest, Transport, TransportError,
    conformance,
};

#[test]
fn the_reference_adapter_conforms() {
    conformance::run_suite(|| MemoryTransport::new("reference")).expect("adapter should conform");
}

/// Builds a message object.
fn message(name: &str) -> PublishObject {
    PublishObject {
        name: name.to_owned(),
        path: format!("messages/2026/09/{name}.age"),
        class: ObjectClass::Message,
        bytes: format!("ciphertext-{name}").into_bytes(),
    }
}

/// Builds a control object.
fn control(name: &str) -> PublishObject {
    PublishObject {
        name: name.to_owned(),
        path: format!("control/log/{name}.json"),
        class: ObjectClass::Control,
        bytes: format!("control-{name}").into_bytes(),
    }
}

/// A created channel with genesis published.
fn created() -> (MemoryTransport, String) {
    let mut transport = MemoryTransport::new("test");
    let genesis = transport.create_group(vec![control("00000000")]).unwrap();
    (transport, genesis.revision)
}

#[test]
fn a_substituted_object_is_detected_rather_than_returned() {
    // Simulates a hostile provider: bytes change under a name the core has
    // already recorded. PRD section 26 requires this to be caught, not served.
    let (mut transport, head) = created();
    let published = transport
        .publish(PublishRequest {
            expected_revision: Some(head),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let recorded_hash = published.objects[0].sha256.clone();
    transport.corrupt_object_for_test("msg-1", b"substituted".to_vec());

    assert!(matches!(
        transport.get_object("msg-1", &recorded_hash),
        Err(TransportError::ObjectHashMismatch { .. })
    ));
}

#[test]
fn a_cursor_that_is_no_longer_in_history_is_reported() {
    // After a history rewrite, a client's saved cursor names a revision the
    // transport can no longer place. Silently restarting from genesis would
    // hide the rewrite; PRD section 17.5.2 requires it to be visible.
    let (mut transport, head) = created();
    let dropped = transport
        .publish(PublishRequest {
            expected_revision: Some(head),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    transport.rewrite_history_for_test();

    assert!(matches!(
        transport.fetch(Some(&dropped.revision), 10),
        Err(TransportError::InvalidPublication { .. })
    ));
}

#[test]
fn republishing_identical_bytes_under_a_name_is_allowed() {
    // Idempotent retry after a lost response: the same object under the same
    // name is the "already published" case from PRD section 17.4, not a
    // conflict.
    let (mut transport, head) = created();
    let first = transport
        .publish(PublishRequest {
            expected_revision: Some(head),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let again = transport.publish(PublishRequest {
        expected_revision: Some(first.revision),
        class: PublicationClass::Data,
        objects: vec![message("msg-1")],
    });

    assert!(again.is_ok(), "identical re-publication should be accepted");
}

#[test]
fn reusing_a_name_with_different_bytes_is_a_security_conflict() {
    // PRD section 17.4: an existing path with a different hash is a security
    // conflict, never an overwrite.
    let (mut transport, head) = created();
    let first = transport
        .publish(PublishRequest {
            expected_revision: Some(head),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let mut different = message("msg-1");
    different.bytes = b"different ciphertext".to_vec();

    assert!(matches!(
        transport.publish(PublishRequest {
            expected_revision: Some(first.revision),
            class: PublicationClass::Data,
            objects: vec![different],
        }),
        Err(TransportError::InvalidPublication { .. })
    ));
}

#[test]
fn an_oversized_object_is_refused() {
    let (mut transport, head) = created();
    let limit = transport.capabilities().max_object_bytes;

    let mut huge = message("blob-1");
    huge.bytes = vec![0u8; limit as usize + 1];

    assert!(matches!(
        transport.publish(PublishRequest {
            expected_revision: Some(head),
            class: PublicationClass::Data,
            objects: vec![huge],
        }),
        Err(TransportError::ObjectTooLarge { .. })
    ));
}

#[test]
fn duplicate_object_names_within_one_publication_are_refused() {
    let (mut transport, head) = created();

    assert!(matches!(
        transport.publish(PublishRequest {
            expected_revision: Some(head),
            class: PublicationClass::Data,
            objects: vec![message("msg-1"), message("msg-1")],
        }),
        Err(TransportError::InvalidPublication { .. })
    ));
}

#[test]
fn publishing_before_the_channel_exists_is_refused() {
    let mut transport = MemoryTransport::new("test");

    assert!(matches!(
        transport.publish(PublishRequest {
            expected_revision: None,
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        }),
        Err(TransportError::NoSuchGroup)
    ));
}

#[test]
fn the_adapter_declares_that_it_is_not_durable() {
    // Honesty about capabilities is the point of the declaration: the core
    // must not infer durability from the fact that an adapter works.
    let transport = MemoryTransport::new("test");
    assert!(!transport.capabilities().durable);
    assert_eq!(
        transport.capabilities().protocol,
        hrc_protocol::TRANSPORT_PROTOCOL_ID
    );
}

/// An adapter that ignores `expected_revision`, the way a naive
/// implementation backed by "just append" would.
///
/// This exists to prove the conformance suite has teeth. A suite that only
/// ever runs against a correct adapter cannot demonstrate that it would
/// catch an incorrect one.
struct LastWriteWinsTransport(MemoryTransport);

impl Transport for LastWriteWinsTransport {
    fn capabilities(&self) -> &hrc_transport::AdapterCapabilities {
        self.0.capabilities()
    }

    fn create_group(
        &mut self,
        objects: Vec<PublishObject>,
    ) -> hrc_transport::Result<hrc_transport::Publication> {
        self.0.create_group(objects)
    }

    fn open_group(&self) -> hrc_transport::Result<hrc_transport::GroupState> {
        self.0.open_group()
    }

    fn publish(
        &mut self,
        mut request: PublishRequest,
    ) -> hrc_transport::Result<hrc_transport::Publication> {
        // The bug: pretend the caller was up to date.
        request.expected_revision = self.0.head();
        self.0.publish(request)
    }

    fn fetch(
        &self,
        after: Option<&str>,
        limit: usize,
    ) -> hrc_transport::Result<hrc_transport::FetchPage> {
        self.0.fetch(after, limit)
    }

    fn get_object(&self, name: &str, expected_sha256: &str) -> hrc_transport::Result<Vec<u8>> {
        self.0.get_object(name, expected_sha256)
    }

    fn health(&self) -> hrc_transport::Result<()> {
        self.0.health()
    }
}

#[test]
fn the_suite_rejects_an_adapter_without_compare_and_swap() {
    let failure = conformance::run_suite(|| LastWriteWinsTransport(MemoryTransport::new("broken")))
        .expect_err("an adapter without compare-and-swap must not pass");

    assert_eq!(failure.check, "publish_is_compare_and_swap");
    assert!(
        !failure.rationale.is_empty(),
        "a failure must explain what the protocol relies on"
    );
}
