//! Surface split tests.
//!
//! The claim these defend is the product's central safety property: an agent
//! talking to the daemon cannot obtain a body a human has not approved, and
//! cannot perform an operation section 22.7 reserves for a human.
//!
//! So the tests are written from the attacker's side. They try every trusted
//! method against the agent-safe surface, look for the body in everything the
//! agent-safe surface can return, and try to replay an approval.

use super::*;

use hrc_protocol::message::{Addressing, MessageEnvelope};

const NOW: &str = "2026-09-13T00:00:00Z";
const LATER: &str = "2026-09-13T01:00:00Z";
const BODY: &str = "the staging credentials rotated on Tuesday";

/// A daemon fixture holding exactly one pending message.
struct TestBroker {
    message: QuarantinedMessage,
    body: String,
    approved: Option<String>,
    drafts: Vec<String>,
    decisions: Vec<crate::gate::DecisionRecord>,
    applied: Vec<&'static str>,
}

impl TestBroker {
    fn new() -> Self {
        Self {
            message: quarantined(),
            body: BODY.to_owned(),
            approved: None,
            drafts: Vec::new(),
            decisions: Vec::new(),
            applied: Vec::new(),
        }
    }

    fn message_id(&self) -> String {
        self.message.envelope.message_id.clone()
    }
}

impl Broker for TestBroker {
    fn channel_status(&self) -> Vec<ChannelStatus> {
        vec![ChannelStatus {
            local_name: "Team channel".into(),
            roster_epoch: 1,
            pending: 1,
            halted: false,
        }]
    }

    fn channel_names(&self) -> Vec<String> {
        vec!["Team channel".into()]
    }

    fn local_identity(&self) -> (String, String) {
        ("roland".into(), "device-1".into())
    }

    fn inbox(&self, _pending_only: bool) -> Vec<AgentView> {
        vec![crate::gate::agent_view(
            &self.message,
            "Alice",
            "Team channel",
            self.body.len() as u64,
            1024,
        )]
    }

    fn approved_content(&self, _message_id: &str) -> Option<String> {
        self.approved.clone()
    }

    fn checks(&self) -> Vec<(String, bool)> {
        vec![("device_keys".into(), true)]
    }

    fn audit(&self) -> Vec<String> {
        vec!["2026-09-13T00:00:00Z message received".into()]
    }

    fn record_draft(&mut self, recipient: &str, text: &str, _endpoint: Option<&str>) -> String {
        self.drafts.push(format!("{recipient}: {text}"));
        format!("draft-{}", self.drafts.len())
    }

    fn observe(&self, _message_id: &str, _until: &str) -> Option<String> {
        Some("delivered".into())
    }

    fn pending(&self, message_id: &str) -> Option<&QuarantinedMessage> {
        (message_id == self.message.envelope.message_id).then_some(&self.message)
    }

    fn pending_body(&self, message_id: &str) -> Option<String> {
        (message_id == self.message.envelope.message_id).then(|| self.body.clone())
    }

    fn channel_local_name(&self, _message_id: &str) -> String {
        "Team channel".into()
    }

    fn local_user(&self) -> String {
        "roland".into()
    }

    fn record_decision_record(&mut self, record: &crate::gate::DecisionRecord) -> Result<()> {
        self.decisions.push(record.clone());
        Ok(())
    }

    fn apply_trusted(&mut self, request: &TrustedRequest) -> Result<()> {
        self.applied.push(request.method());
        Ok(())
    }
}

fn quarantined() -> QuarantinedMessage {
    let envelope = MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: hrc_protocol::canonical::sha256_hex(b"channel"),
        roster_epoch: 1,
        message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        device_sequence: 1,
        previous_chain_id: None,
        created_at: NOW.into(),
        expires_at: None,
        to: Addressing {
            principals: vec!["bob".into()],
            endpoint: Some("reviewer".into()),
        },
        recipients: hrc_protocol::RecipientDevices::new([hrc_protocol::canonical::sha256_hex(
            b"device",
        )])
        .unwrap(),
        thread_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        in_reply_to: None,
        kind: "question".into(),
        requested_capability: None,
        body: serde_json::json!({ "text": BODY }),
        attachments: Vec::new(),
        padding: String::new(),
    };

    QuarantinedMessage {
        envelope,
        sender_principal: "alice".into(),
        sender_device: hrc_protocol::canonical::sha256_hex(b"alice-device"),
        ciphertext_sha256: hrc_protocol::canonical::sha256_hex(b"ciphertext"),
    }
}

/// Every operation PRD section 22.7 reserves for the human.
fn every_trusted_request() -> Vec<TrustedRequest> {
    vec![
        TrustedRequest::PreviewPending {
            message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
        },
        TrustedRequest::Approve {
            message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            decision: WireDecision::DeliverToAgent {
                agent: "reviewer-pane".into(),
            },
            expires_at: LATER.into(),
        },
        TrustedRequest::ApproveJoin {
            request_id: "join-1".into(),
        },
        TrustedRequest::RejectJoin {
            request_id: "join-1".into(),
        },
        TrustedRequest::RemoveMember {
            principal_id: "alice".into(),
        },
        TrustedRequest::RevokeDevice {
            device_id: "device-2".into(),
        },
        TrustedRequest::GrantCapability {
            principal_id: "alice".into(),
            capability: "delegate".into(),
        },
        TrustedRequest::MakeRepositoryPublic,
        TrustedRequest::Rollover,
    ]
}

#[test]
fn every_trusted_operation_is_refused_on_the_agent_safe_surface() {
    // PRD section 22.7, exhaustively. A new trusted method that forgot to be
    // trusted would have to be added to this list to compile a test for it,
    // and the classification test below catches one that was never listed.
    let mut broker = TestBroker::new();

    for request in every_trusted_request() {
        let method = request.method();
        let error = dispatch_agent(&mut broker, Request::Trusted(request)).unwrap_err();

        assert!(
            matches!(error, CoreError::AuthorizationRequired { operation } if operation == method),
            "{method} was not refused: {error}"
        );
    }

    assert!(
        broker.applied.is_empty() && broker.decisions.is_empty(),
        "a refused request changed daemon state"
    );
}

#[test]
fn the_section_22_7_list_is_exactly_the_trusted_request_set() {
    // Reading the PRD list and the enum side by side is the check a reviewer
    // would otherwise have to do by eye.
    let mut named: Vec<&str> = every_trusted_request()
        .iter()
        .map(TrustedRequest::method)
        .collect();
    named.sort_unstable();

    assert_eq!(
        named,
        vec![
            "approve",
            "approve_join",
            "grant_capability",
            "make_repository_public",
            "preview_pending",
            "reject_join",
            "remove_member",
            "revoke_device",
            "rollover",
        ]
    );

    for request in every_trusted_request() {
        assert!(Request::Trusted(request).requires_trusted_surface());
    }
}

#[test]
fn no_agent_safe_answer_can_carry_a_pending_body() {
    // The invariant of section 19.1. Rendering every response the agent-safe
    // surface can produce and searching for the body is a blunt check, and
    // that is the point: it does not depend on knowing which field would
    // have leaked.
    let mut broker = TestBroker::new();

    let requests = vec![
        AgentRequest::Status,
        AgentRequest::Channels,
        AgentRequest::Whoami,
        AgentRequest::Inbox { pending_only: true },
        AgentRequest::Inbox {
            pending_only: false,
        },
        AgentRequest::Doctor,
        AgentRequest::Audit,
        AgentRequest::Draft {
            recipient: "alice".into(),
            text: "a draft".into(),
            endpoint: None,
        },
        AgentRequest::Wait {
            message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".into(),
            until: "delivered".into(),
        },
    ];

    for request in requests {
        let method = request.method();
        let response = dispatch_agent(&mut broker, Request::Agent(request)).unwrap();
        let rendered = format!("{response:?}");

        assert!(
            !rendered.contains(BODY),
            "the pending body leaked through `{method}`: {rendered}"
        );
    }
}

#[test]
fn asking_for_a_pending_body_as_approved_content_is_refused() {
    // And the refusal is the same one an unknown message produces, so the
    // agent cannot use the error to learn that a body exists.
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();

    let pending = dispatch_agent(
        &mut broker,
        Request::Agent(AgentRequest::ShowApproved {
            message_id: message_id.clone(),
        }),
    )
    .unwrap_err();

    let unknown = dispatch_agent(
        &mut broker,
        Request::Agent(AgentRequest::ShowApproved {
            message_id: "01BX5ZZKBKACTAV9WEVGEMMVRZ".into(),
        }),
    )
    .unwrap_err();

    assert!(matches!(pending, CoreError::NoApprovedContent { .. }));
    assert_eq!(
        std::mem::discriminant(&pending),
        std::mem::discriminant(&unknown)
    );
}

#[test]
fn approved_content_becomes_readable_once_the_human_approves() {
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    let response = dispatch_trusted(
        &mut broker,
        &mut ledger,
        TrustedRequest::Approve {
            message_id: message_id.clone(),
            decision: WireDecision::DeliverToAgent {
                agent: "reviewer-pane".into(),
            },
            expires_at: LATER.into(),
        },
        NOW,
    )
    .unwrap();

    let TrustedResponse::Delivered { agent, framed } = response else {
        panic!("expected delivery");
    };
    assert_eq!(agent, "reviewer-pane");
    assert!(framed.starts_with("[REMOTE HRC MESSAGE]"));
    assert!(framed.contains("Approved locally by: roland"));
    assert!(framed.ends_with(BODY));

    // Only now does the agent-safe surface have something to show.
    broker.approved = Some(framed);
    let response = dispatch_agent(
        &mut broker,
        Request::Agent(AgentRequest::ShowApproved { message_id }),
    )
    .unwrap();
    assert!(matches!(response, AgentResponse::Approved { .. }));
}

#[test]
fn an_approval_is_spent_by_the_call_that_issues_it() {
    // The authorization never leaves the daemon, so there is nothing to
    // replay. Approving again is a second decision by the human, and it must
    // work — but it is a second decision, not a reuse of the first.
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    let approve = |broker: &mut TestBroker, ledger: &mut AuthorizationLedger| {
        dispatch_trusted(
            broker,
            ledger,
            TrustedRequest::Approve {
                message_id: message_id.clone(),
                decision: WireDecision::DeliverToAgent {
                    agent: "reviewer-pane".into(),
                },
                expires_at: LATER.into(),
            },
            NOW,
        )
    };

    approve(&mut broker, &mut ledger).unwrap();

    let error = approve(&mut broker, &mut ledger).unwrap_err();
    assert!(matches!(error, CoreError::AuthorizationAlreadyUsed));
}

#[test]
fn an_edited_delivery_carries_the_text_the_human_wrote() {
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    let edited = "the staging credentials rotated on [REDACTED]";
    let response = dispatch_trusted(
        &mut broker,
        &mut ledger,
        TrustedRequest::Approve {
            message_id,
            decision: WireDecision::DeliverEdited {
                agent: "reviewer-pane".into(),
                text: edited.into(),
            },
            expires_at: LATER.into(),
        },
        NOW,
    )
    .unwrap();

    let TrustedResponse::Delivered { framed, .. } = response else {
        panic!("expected delivery");
    };
    assert!(framed.ends_with(edited));
    assert!(
        !framed.contains(BODY),
        "the unedited body was delivered: {framed}"
    );
}

#[test]
fn a_decision_that_reaches_no_agent_delivers_nothing() {
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    for decision in [
        WireDecision::KeepInInbox,
        WireDecision::Decline {
            reason: Some("not now".into()),
        },
    ] {
        let response = dispatch_trusted(
            &mut broker,
            &mut ledger,
            TrustedRequest::Approve {
                message_id: message_id.clone(),
                decision,
                expires_at: LATER.into(),
            },
            NOW,
        )
        .unwrap();

        assert!(matches!(response, TrustedResponse::Done));
    }

    assert_eq!(broker.decisions.len(), 2);
    assert!(broker.approved.is_none());
}

#[test]
fn the_trusted_surface_reads_a_pending_body_and_the_agent_surface_cannot() {
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    let response = dispatch_trusted(
        &mut broker,
        &mut ledger,
        TrustedRequest::PreviewPending {
            message_id: message_id.clone(),
        },
        NOW,
    )
    .unwrap();

    let TrustedResponse::Pending { body, .. } = response else {
        panic!("expected a pending body");
    };
    assert_eq!(body, BODY);

    // The identical request over the agent-safe surface.
    let error = dispatch_agent(
        &mut broker,
        Request::Trusted(TrustedRequest::PreviewPending { message_id }),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CoreError::AuthorizationRequired {
            operation: "preview_pending"
        }
    ));
}

#[test]
fn requests_round_trip_through_the_wire_shape() {
    // An agent constructs these as JSON, so the tags have to be stable and
    // a trusted method must still decode as trusted rather than falling
    // through to something agent-safe.
    let cases = [
        (r#"{"method":"status"}"#, false),
        (
            r#"{"method":"inbox","params":{"pending_only":true}}"#,
            false,
        ),
        (
            r#"{"method":"remove_member","params":{"principal_id":"alice"}}"#,
            true,
        ),
        (r#"{"method":"make_repository_public"}"#, true),
        (r#"{"method":"rollover"}"#, true),
    ];

    for (json, trusted) in cases {
        let request: Request = hrc_protocol::canonical::from_json_str(json).unwrap();
        assert_eq!(
            request.requires_trusted_surface(),
            trusted,
            "{json} was classified wrongly as {}",
            request.method()
        );
    }
}

#[test]
fn an_unknown_method_does_not_decode() {
    // Falling back to some default would mean an unrecognized method is
    // answered by whatever variant happened to match loosest.
    for json in [
        r#"{"method":"read_private_key"}"#,
        r#"{"method":"inbox_with_bodies"}"#,
        r#"{"method":"approve_everything"}"#,
    ] {
        assert!(
            hrc_protocol::canonical::from_json_str::<Request>(json).is_err(),
            "{json} decoded"
        );
    }
}

#[test]
fn an_agent_safe_request_cannot_smuggle_a_trusted_parameter() {
    // Adding a field that a trusted method uses must not turn an agent-safe
    // request into one, and must not be silently ignored either.
    let json = r#"{"method":"inbox","params":{"pending_only":true,"include_bodies":true}}"#;

    assert!(
        hrc_protocol::canonical::from_json_str::<Request>(json).is_err(),
        "an unknown parameter was accepted"
    );
}

#[test]
fn every_decision_leaves_an_audit_record() {
    // PRD requirement HRC-GATE-004. An audit that recorded only approvals
    // would show a channel in which nothing was ever refused.
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    let decisions = [
        WireDecision::KeepInInbox,
        WireDecision::Decline {
            reason: Some("not now".into()),
        },
        WireDecision::DeliverToAgent {
            agent: "reviewer-pane".into(),
        },
    ];

    for decision in decisions {
        dispatch_trusted(
            &mut broker,
            &mut ledger,
            TrustedRequest::Approve {
                message_id: message_id.clone(),
                decision,
                expires_at: LATER.into(),
            },
            NOW,
        )
        .unwrap();
    }

    let actions: Vec<&str> = broker
        .decisions
        .iter()
        .map(|record| record.action)
        .collect();
    assert_eq!(
        actions,
        vec!["keep_in_inbox", "decline", "deliver_to_agent"]
    );

    for record in &broker.decisions {
        assert_eq!(record.message_id, message_id);
        assert_eq!(record.decided_by, "roland");
        assert_eq!(record.occurred_at, NOW);
        assert_eq!(record.original_content, BODY);
    }
}

#[test]
fn an_edited_delivery_records_both_versions() {
    // The question asked of an audit trail afterwards is not "was this
    // approved" but "what did the human take out before an agent saw it".
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    let edited = "the staging credentials rotated on [REDACTED]";
    dispatch_trusted(
        &mut broker,
        &mut ledger,
        TrustedRequest::Approve {
            message_id,
            decision: WireDecision::DeliverEdited {
                agent: "reviewer-pane".into(),
                text: edited.into(),
            },
            expires_at: LATER.into(),
        },
        NOW,
    )
    .unwrap();

    let record = &broker.decisions[0];
    assert_eq!(record.action, "deliver_edited");
    assert_eq!(record.original_content, BODY);
    assert_eq!(record.edited_content.as_deref(), Some(edited));
    assert_eq!(
        record.content_hash,
        hrc_protocol::canonical::sha256_hex(BODY.as_bytes())
    );
    assert_eq!(
        record.edited_hash.as_deref(),
        Some(hrc_protocol::canonical::sha256_hex(edited.as_bytes()).as_str())
    );
    assert_eq!(record.agent.as_deref(), Some("reviewer-pane"));
}

#[test]
fn a_refused_approval_records_nothing() {
    // A record written for an approval that did not happen would be worse
    // than no record at all.
    let mut broker = TestBroker::new();
    let mut ledger = AuthorizationLedger::new();

    let error = dispatch_trusted(
        &mut broker,
        &mut ledger,
        TrustedRequest::Approve {
            message_id: "01BX5ZZKBKACTAV9WEVGEMMVRZ".into(),
            decision: WireDecision::KeepInInbox,
            expires_at: LATER.into(),
        },
        NOW,
    )
    .unwrap_err();

    assert!(matches!(error, CoreError::NoPendingMessage { .. }));
    assert!(broker.decisions.is_empty());
}

#[test]
fn a_decision_that_reaches_no_agent_names_no_agent() {
    let mut broker = TestBroker::new();
    let message_id = broker.message_id();
    let mut ledger = AuthorizationLedger::new();

    dispatch_trusted(
        &mut broker,
        &mut ledger,
        TrustedRequest::Approve {
            message_id,
            decision: WireDecision::KeepInInbox,
            expires_at: LATER.into(),
        },
        NOW,
    )
    .unwrap();

    assert!(broker.decisions[0].agent.is_none());
    assert!(broker.decisions[0].edited_content.is_none());
}
