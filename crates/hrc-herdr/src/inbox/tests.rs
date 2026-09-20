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

#[test]
fn a_channel_name_that_is_a_git_url_is_shortened_for_display() {
    // Found on a real screen, not in a test: `hrc create` records the
    // locator as the channel's name, and DEC-081 made that locator a full
    // URL, which ran past the pane border and broke the frame.
    assert_eq!(
        channel_display_name("https://github.com/czinegeroland/hrc-test.git"),
        "czinegeroland/hrc-test"
    );
    assert_eq!(
        channel_display_name("https://github.com/owner/name"),
        "owner/name"
    );
    assert_eq!(
        channel_display_name("git@github.com:owner/name.git"),
        "owner/name"
    );
    assert_eq!(
        channel_display_name("ssh://git@example.com/team/repo.git"),
        "team/repo"
    );
}

#[test]
fn a_local_repository_is_named_by_its_directory() {
    assert_eq!(
        channel_display_name(r"C:\claude_working_directory\remote_messaging_test"),
        "remote_messaging_test"
    );
    assert_eq!(channel_display_name("/srv/hrc/channel.git"), "channel");
    assert_eq!(channel_display_name("/srv/hrc/channel.git/"), "channel");
}

#[test]
fn a_name_a_person_chose_is_left_alone() {
    // The shortening exists for names nobody chose. One somebody did choose
    // is theirs, however it is spelled.
    for name in ["Team channel", "hrc", "owner/name"] {
        assert_eq!(channel_display_name(name), name);
    }
}

#[test]
fn an_assigned_name_stands_in_for_the_principal() {
    assert_eq!(
        principal_display_name("PpWNIUyibQ3l8xK2", Some("Alice")),
        "Alice"
    );
}

#[test]
fn an_unnamed_principal_is_shortened_and_marked() {
    // The trailing marker is deliberate: it has to be visible that this is
    // not the whole identifier, and it must not look like part of one.
    assert_eq!(
        principal_display_name("PpWNIUyibQ3l8xK2", None),
        "PpWNIUyi~"
    );
}

#[test]
fn a_short_principal_is_not_marked_as_truncated() {
    assert_eq!(principal_display_name("abc", None), "abc");
    assert_eq!(principal_display_name("abcdefgh", None), "abcdefgh");
}

#[test]
fn a_blank_alias_falls_back_rather_than_rendering_nothing() {
    // Storage refuses to write one, but a row with no label at all cannot be
    // selected, so the renderer does not depend on that refusal.
    assert_eq!(
        principal_display_name("PpWNIUyibQ3l8xK2", Some("   ")),
        "PpWNIUyi~"
    );
}

#[test]
fn shortening_a_principal_respects_character_boundaries() {
    // A principal is base64 in practice, but the renderer must not be the
    // thing that panics if one ever is not.
    assert_eq!(principal_display_name("ééééééééééé", None), "éééééééé~");
}
