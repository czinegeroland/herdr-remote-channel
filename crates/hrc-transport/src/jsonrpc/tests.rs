use super::*;

use crate::memory::MemoryTransport;
use crate::{FetchPage, GroupState};

/// Serves one request against a fresh in-memory adapter and returns the raw
/// response. In-process, because these tests are about the framing and the
/// error mapping rather than about the process boundary — the conformance
/// run in `tests/out_of_process_adapter.rs` covers that.
fn round_trip(requests: &[RpcRequest]) -> Vec<RpcResponse> {
    let mut input = Vec::new();
    for request in requests {
        write_frame(&mut input, request).expect("a request frames");
    }

    let mut transport = MemoryTransport::new("test");
    let mut output = Vec::new();
    serve(&mut transport, input.as_slice(), &mut output).expect("the loop should finish cleanly");

    let mut reader = output.as_slice();
    let mut responses = Vec::new();
    while let Some(response) = read_frame::<_, RpcResponse>(&mut reader).expect("a response parses")
    {
        responses.push(response);
    }
    responses
}

fn request(id: u64, method: &str, params: Option<serde_json::Value>) -> RpcRequest {
    RpcRequest {
        jsonrpc: "2.0".into(),
        id,
        method: method.to_owned(),
        params,
    }
}

#[test]
fn a_frame_round_trips_through_its_length_prefix() {
    let mut buffer = Vec::new();
    let original = request(7, method::HEALTH, None);
    write_frame(&mut buffer, &original).expect("write");

    // Four bytes of big-endian length, then that many bytes of JSON.
    let declared = u32::from_be_bytes([buffer[0], buffer[1], buffer[2], buffer[3]]) as usize;
    assert_eq!(declared, buffer.len() - 4);

    let mut reader = buffer.as_slice();
    let parsed: RpcRequest = read_frame(&mut reader).expect("read").expect("a frame");
    assert_eq!(parsed.id, 7);
    assert_eq!(parsed.method, method::HEALTH);
}

#[test]
fn a_clean_end_of_stream_is_not_an_error() {
    // How the server loop learns the caller is gone, rather than by failing.
    let mut empty: &[u8] = &[];
    let frame: Option<RpcRequest> = read_frame(&mut empty).expect("end of stream is clean");
    assert!(frame.is_none());
}

#[test]
fn an_oversized_frame_is_refused_before_anything_is_allocated() {
    // The length is checked against the limit rather than trusted, so a peer
    // never decides how much memory this process reserves.
    let header = u32::try_from(MAX_FRAME_BYTES + 1).expect("fits in u32");
    let mut input = header.to_be_bytes().to_vec();
    input.extend_from_slice(b"not actually this long");

    let mut reader = input.as_slice();
    let error =
        read_frame::<_, RpcRequest>(&mut reader).expect_err("an oversized frame is refused");

    assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn initialize_answers_with_the_declared_capabilities() {
    let responses = round_trip(&[request(1, method::INITIALIZE, None)]);

    let result = responses[0].result.as_ref().expect("a result");
    assert_eq!(result["historyModel"], "linear_append_only");
    assert_eq!(result["protocol"], hrc_protocol::TRANSPORT_PROTOCOL_ID);
    assert!(result["maxObjectBytes"].as_u64().unwrap() > 0);
}

#[test]
fn an_unknown_method_is_reported_rather_than_ignored() {
    let responses = round_trip(&[request(1, "publish_everything", None)]);

    let error = responses[0].error.as_ref().expect("an error");
    assert_eq!(error.code, code::METHOD_NOT_FOUND);
}

#[test]
fn a_method_missing_its_parameters_is_an_invalid_params_error() {
    let responses = round_trip(&[request(1, method::PUBLISH, None)]);

    let error = responses[0].error.as_ref().expect("an error");
    assert_eq!(error.code, code::INVALID_PARAMS);
}

#[test]
fn an_error_does_not_end_the_loop() {
    // A conflict or a missing object is an ordinary answer. Tearing down the
    // process the core depends on because one call failed would turn every
    // recoverable outcome into an outage.
    let responses = round_trip(&[
        request(1, "nonsense", None),
        request(2, method::INITIALIZE, None),
    ]);

    assert_eq!(responses.len(), 2);
    assert!(responses[0].error.is_some());
    assert!(responses[1].result.is_some());
}

#[test]
fn shutdown_is_answered_and_then_the_loop_ends() {
    let responses = round_trip(&[
        request(1, method::SHUTDOWN, None),
        request(2, method::INITIALIZE, None),
    ]);

    assert_eq!(responses.len(), 1, "nothing is served after shutdown");
    assert!(responses[0].result.is_some());
}

#[test]
fn every_response_carries_the_id_of_its_request() {
    let responses = round_trip(&[
        request(11, method::INITIALIZE, None),
        request(22, method::HEALTH, None),
    ]);

    assert_eq!(responses[0].id, 11);
    assert_eq!(responses[1].id, 22);
}

#[test]
fn every_transport_error_maps_to_a_code_and_back_to_itself() {
    // The mapping is a round trip, not a one-way flattening. A code that
    // decoded to the wrong variant would make the core retry something it
    // should stop on, or stop on something it should retry.
    let cases = vec![
        TransportError::Conflict {
            current: Some("rev-2".into()),
        },
        TransportError::NoSuchGroup,
        TransportError::GroupExists,
        TransportError::NoSuchObject {
            name: "messages/a.age".into(),
        },
        TransportError::ObjectHashMismatch {
            name: "messages/a.age".into(),
        },
        TransportError::InvalidPublication {
            reason: "empty".into(),
        },
        TransportError::ObjectTooLarge {
            name: "blobs/b.age".into(),
            size: 99,
            limit: 10,
        },
        TransportError::Provider("the provider fell over".into()),
    ];

    for original in cases {
        let rebuilt = error_from_object(error_object(&original));

        assert_eq!(
            std::mem::discriminant(&original),
            std::mem::discriminant(&rebuilt),
            "{original:?} came back as {rebuilt:?}"
        );

        // The fields a caller acts on must survive, not just the variant.
        match (&original, &rebuilt) {
            (
                TransportError::Conflict { current: before },
                TransportError::Conflict { current: after },
            ) => assert_eq!(before, after, "a conflict must keep its revision"),
            (
                TransportError::ObjectTooLarge {
                    size: before_size,
                    limit: before_limit,
                    ..
                },
                TransportError::ObjectTooLarge {
                    size: after_size,
                    limit: after_limit,
                    ..
                },
            ) => {
                assert_eq!(before_size, after_size);
                assert_eq!(before_limit, after_limit);
            }
            _ => {}
        }
    }
}

#[test]
fn an_unrecognized_error_code_becomes_a_provider_error() {
    // Not a guess. Mapping an unknown code onto `Conflict` would make the
    // core retry forever against an adapter that failed in a new way.
    let rebuilt = error_from_object(RpcError {
        code: 4242,
        message: "something this build has never heard of".into(),
        data: None,
    });

    assert!(
        matches!(rebuilt, TransportError::Provider(_)),
        "{rebuilt:?}"
    );
}

#[test]
fn a_publication_class_the_adapter_invents_is_refused() {
    let wire = WirePublication {
        revision: "rev-1".into(),
        parent_revision: None,
        class: "whatever".into(),
        transport_time: "2026-09-13T00:00:00Z".into(),
        objects: Vec::new(),
    };

    assert!(wire.into_publication().is_err());
}

#[test]
fn an_object_class_the_adapter_invents_is_refused() {
    let wire = WireObject {
        name: "messages/a.age".into(),
        class: "executable".into(),
        size: 1,
        sha256: "0".repeat(64),
        bytes: None,
    };

    assert!(wire.into_record().is_err());
}

/// An adapter that declares push support and answers `wait` for real.
///
/// Neither shipped adapter is push-capable — a Git remote has no way to
/// notify a client — so the waiting half of requirement HRC-TR-003 would
/// otherwise have nothing to exercise it.
struct WaitingTransport {
    inner: MemoryTransport,
    capabilities: AdapterCapabilities,
}

impl WaitingTransport {
    fn new() -> Self {
        let inner = MemoryTransport::new("waiting");
        let mut capabilities = inner.capabilities().clone();
        capabilities.supports_wait = true;

        Self {
            inner,
            capabilities,
        }
    }
}

impl Transport for WaitingTransport {
    fn capabilities(&self) -> &AdapterCapabilities {
        &self.capabilities
    }

    fn create_group(&mut self, objects: Vec<PublishObject>) -> Result<Publication> {
        self.inner.create_group(objects)
    }

    fn open_group(&self) -> Result<GroupState> {
        self.inner.open_group()
    }

    fn publish(&mut self, request: PublishRequest) -> Result<Publication> {
        self.inner.publish(request)
    }

    fn fetch(&self, after: Option<&str>, limit: usize) -> Result<FetchPage> {
        self.inner.fetch(after, limit)
    }

    fn get_object(&self, name: &str, expected_sha256: &str) -> Result<Vec<u8>> {
        self.inner.get_object(name, expected_sha256)
    }

    fn health(&self) -> Result<()> {
        self.inner.health()
    }

    fn wait(
        &self,
        current: Option<&str>,
        _timeout: std::time::Duration,
    ) -> Result<crate::WaitOutcome> {
        // Nothing else can move an in-process head while this call is on the
        // stack, so there is nothing to block on: the answer is already
        // known. Blocking anyway would only make the test slow.
        match self.inner.open_group() {
            Ok(state) => match state.revision {
                Some(head) if Some(head.as_str()) != current => {
                    Ok(crate::WaitOutcome::Changed(head))
                }
                _ => Ok(crate::WaitOutcome::TimedOut),
            },
            Err(TransportError::NoSuchGroup) => Ok(crate::WaitOutcome::TimedOut),
            Err(error) => Err(error),
        }
    }
}

/// Serves one exchange against a push-capable adapter.
fn wait_round_trip(requests: &[RpcRequest]) -> Vec<RpcResponse> {
    let mut input = Vec::new();
    for request in requests {
        write_frame(&mut input, request).expect("a request frames");
    }

    let mut transport = WaitingTransport::new();
    let mut output = Vec::new();
    serve(&mut transport, input.as_slice(), &mut output).expect("the loop should finish cleanly");

    let mut reader = output.as_slice();
    let mut responses = Vec::new();
    while let Some(response) = read_frame::<_, RpcResponse>(&mut reader).expect("a response parses")
    {
        responses.push(response);
    }
    responses
}

#[test]
fn a_push_capable_adapter_reports_a_changed_revision_over_the_wire() {
    let genesis = serde_json::json!({
        "objects": [{
            "name": "protocol.json",
            "class": "protocol",
            "size": 2,
            "sha256": hrc_protocol::canonical::sha256_hex(b"{}"),
            "bytes": hrc_protocol::canonical::encode_base64url(b"{}"),
        }]
    });

    let responses = wait_round_trip(&[
        request(1, method::GROUP_CREATE, Some(genesis)),
        request(
            2,
            method::WAIT,
            Some(serde_json::json!({ "revision": null, "timeoutSeconds": 1 })),
        ),
    ]);

    let waited = responses[1].result.as_ref().expect("a wait result");
    assert_eq!(waited["outcome"], "changed");
    assert!(waited["revision"].is_string());
}

#[test]
fn a_quiet_channel_times_out_rather_than_reporting_a_change() {
    let genesis = serde_json::json!({
        "objects": [{
            "name": "protocol.json",
            "class": "protocol",
            "size": 2,
            "sha256": hrc_protocol::canonical::sha256_hex(b"{}"),
            "bytes": hrc_protocol::canonical::encode_base64url(b"{}"),
        }]
    });

    let created = wait_round_trip(&[request(1, method::GROUP_CREATE, Some(genesis.clone()))]);
    let head = created[0].result.as_ref().expect("a publication")["revision"]
        .as_str()
        .expect("a revision")
        .to_owned();

    let responses = wait_round_trip(&[
        request(1, method::GROUP_CREATE, Some(genesis)),
        request(
            2,
            method::WAIT,
            Some(serde_json::json!({ "revision": head, "timeoutSeconds": 1 })),
        ),
    ]);

    assert_eq!(
        responses[1].result.as_ref().expect("a wait result")["outcome"],
        "timed_out"
    );
}

#[test]
fn an_adapter_without_push_support_reports_unsupported() {
    // Distinct from a timeout on purpose: a caller that could not tell them
    // apart would sit through the whole timeout before every poll.
    let responses = round_trip(&[request(
        1,
        method::WAIT,
        Some(serde_json::json!({ "revision": null, "timeoutSeconds": 1 })),
    )]);

    assert_eq!(
        responses[0].result.as_ref().expect("a wait result")["outcome"],
        "unsupported"
    );
}

#[test]
fn a_wait_without_a_timeout_is_an_invalid_params_error() {
    let responses = round_trip(&[request(
        1,
        method::WAIT,
        Some(serde_json::json!({ "revision": null })),
    )]);

    assert_eq!(
        responses[0].error.as_ref().expect("an error").code,
        code::INVALID_PARAMS
    );
}

#[test]
fn a_push_capable_adapter_passes_the_conformance_suite_too() {
    // The suite's declaration check has both directions to exercise only if
    // something actually declares push support.
    crate::conformance::run_suite(WaitingTransport::new).expect("it should conform");
}
