use super::*;

/// A well-formed ULID, so rows carry the identifier shape they will see.
const MESSAGE: &str = "01ARZ3NDEKTSV4RRFFQ69G5FAV";

fn view() -> AgentView {
    AgentView {
        sender_principal: "principal-alice".to_owned(),
        sender_local_name: "Alice".to_owned(),
        kind: "note".to_owned(),
        channel_local_name: "Team channel".to_owned(),
        endpoint_label: "review".to_owned(),
        ciphertext_bytes: 1024,
        plaintext_bytes: 512,
        created_at: "2026-01-01T00:00:00Z".to_owned(),
        arrival_at: "2026-01-01T00:00:05Z".to_owned(),
        expires_at: None,
        thread_label: "01ARYZ6S41000000000000000A".to_owned(),
        prompt_request: false,
        attachment_count: 2,
        attachment_bytes: 4096,
        awaiting_decision: true,
    }
}

#[test]
fn a_row_shows_every_field_section_23_2_requires() {
    let row = InboxRow::from_view(&view(), MESSAGE, "2026-01-01T00:00:06Z");

    // Verified sender identity, type, thread, arrival, expiration,
    // requested endpoint, attachment count and total size, verification and
    // secret-scan status, available local decisions.
    assert_eq!(row.sender_principal, "principal-alice");
    assert_eq!(row.sender_local_name, "Alice");
    assert_eq!(row.kind, "note");
    assert_eq!(row.thread_label, "01ARYZ6S41000000000000000A");
    assert_eq!(row.arrival_at, "2026-01-01T00:00:05Z");
    assert_eq!(row.expires_at, None);
    assert_eq!(row.endpoint_label, "review");
    assert_eq!(row.attachment_count, 2);
    assert_eq!(row.attachment_bytes, 4096);
    assert_eq!(row.verification, Verification::Verified);
    assert_eq!(row.secret_scan, SecretScan::NotApplicable);
    assert_eq!(row.decisions.len(), 4);
}

#[test]
fn an_expired_message_is_shown_as_expired_and_offers_no_decision() {
    // PRD section 26: display as expired but do not allow action.
    let mut view = view();
    view.expires_at = Some("2026-01-01T00:00:04Z".to_owned());

    let row = InboxRow::from_view(&view, MESSAGE, "2026-01-01T00:00:06Z");

    assert_eq!(row.verification, Verification::Expired);
    assert!(row.decisions.is_empty());
    assert!(!row.awaiting_decision());
}

#[test]
fn expiry_is_inclusive_so_a_message_is_not_actionable_at_its_own_deadline() {
    let mut view = view();
    view.expires_at = Some("2026-01-01T00:00:06Z".to_owned());

    let row = InboxRow::from_view(&view, MESSAGE, "2026-01-01T00:00:06Z");

    assert_eq!(row.verification, Verification::Expired);
}

#[test]
fn a_decided_message_stays_visible_but_offers_nothing_further() {
    let mut view = view();
    view.awaiting_decision = false;

    let row = InboxRow::from_view(&view, MESSAGE, "2026-01-01T00:00:06Z");

    assert_eq!(row.verification, Verification::Verified);
    assert!(row.decisions.is_empty());
}

#[test]
fn the_four_decisions_are_the_ones_section_19_2_lists() {
    let row = InboxRow::from_view(&view(), MESSAGE, "2026-01-01T00:00:06Z");

    assert_eq!(
        row.decisions,
        vec![
            LocalDecision::DeliverToAgent,
            LocalDecision::DeliverEdited,
            LocalDecision::KeepInInbox,
            LocalDecision::Decline,
        ]
    );
}

#[test]
fn rows_are_ordered_by_when_they_arrived_here() {
    // Not by the sender's stated creation time: that is sender-controlled,
    // and a sender could otherwise decide where their message sits in
    // someone else's inbox.
    let mut early = view();
    early.arrival_at = "2026-01-01T00:00:01Z".to_owned();
    early.sender_local_name = "First".to_owned();

    let mut late = view();
    late.arrival_at = "2026-01-01T00:00:09Z".to_owned();
    late.sender_local_name = "Second".to_owned();

    let inbox = InboxView::new(vec![
        InboxRow::from_view(&late, "01ARZ3NDEKTSV4RRFFQ69G5FBV", "2026-01-01T00:01:00Z"),
        InboxRow::from_view(&early, MESSAGE, "2026-01-01T00:01:00Z"),
    ]);

    assert_eq!(inbox.rows[0].sender_local_name, "First");
    assert_eq!(inbox.rows[1].sender_local_name, "Second");
}

#[test]
fn pending_counts_only_rows_that_still_need_a_decision() {
    let mut decided = view();
    decided.awaiting_decision = false;

    let inbox = InboxView::new(vec![
        InboxRow::from_view(&view(), MESSAGE, "2026-01-01T00:01:00Z"),
        InboxRow::from_view(
            &decided,
            "01ARZ3NDEKTSV4RRFFQ69G5FBV",
            "2026-01-01T00:01:00Z",
        ),
    ]);

    assert_eq!(inbox.rows.len(), 2);
    assert_eq!(inbox.pending(), 1);
}

#[test]
fn a_rendered_row_carries_no_field_that_could_hold_a_body() {
    // The row is the plugin's whole inbox surface. Serializing it and
    // finding no body is the same argument the agent-safe response makes in
    // hrc-core: the invariant is a property of the type, so a future field
    // that broke it would fail here.
    let row = InboxRow::from_view(&view(), MESSAGE, "2026-01-01T00:00:06Z");
    let rendered = serde_json::to_value(&row).expect("a row serializes");

    let object = rendered.as_object().expect("a row is a JSON object");
    for forbidden in ["body", "text", "content", "plaintext", "subject", "summary"] {
        assert!(
            !object.contains_key(forbidden),
            "an inbox row must not carry `{forbidden}`"
        );
    }
}
