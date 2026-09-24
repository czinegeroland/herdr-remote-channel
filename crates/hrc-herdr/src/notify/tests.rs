use super::*;

use crate::inbox::{InboxDisposition, InboxRow, LocalDecision, SecretScan, Verification};

fn row(kind: &str, prompt_request: bool) -> InboxRow {
    InboxRow {
        message_id: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        message_label: "01ARZ3NDEKTSV4RRFFQ69G5FAV".to_owned(),
        sender_principal: "principal-alice".to_owned(),
        sender_local_name: "Alice".to_owned(),
        kind: kind.to_owned(),
        thread_label: "01ARYZ6S41000000000000000A".to_owned(),
        channel_local_name: "Team channel".to_owned(),
        arrival_at: "2026-01-01T00:00:05Z".to_owned(),
        expires_at: None,
        endpoint_label: "review".to_owned(),
        prompt_request,
        attachment_count: 0,
        attachment_bytes: 0,
        verification: Verification::Verified,
        secret_scan: SecretScan::NotApplicable,
        disposition: InboxDisposition::Pending,
        decisions: vec![LocalDecision::KeepInInbox],
    }
}

#[test]
fn a_question_and_a_task_each_raise_their_own_notification() {
    assert!(matches!(
        Notification::for_message(&row("question", false)),
        Some(Notification::NewQuestion { .. })
    ));
    assert!(matches!(
        Notification::for_message(&row("task", false)),
        Some(Notification::NewTaskRequest { .. })
    ));
}

#[test]
fn a_prompt_request_is_a_capability_rather_than_a_kind() {
    // `prompt:request` travels as a requested capability on an ordinary
    // message, so a note carrying it is still a prompt request.
    assert!(matches!(
        Notification::for_message(&row("note", true)),
        Some(Notification::NewPromptRequest { .. })
    ));
}

#[test]
fn the_prompt_capability_outranks_the_kind_it_arrived_on() {
    // A task that also asks for the prompt capability blocks on a human, so
    // it must not be announced as an ordinary task request.
    assert!(matches!(
        Notification::for_message(&row("task", true)),
        Some(Notification::NewPromptRequest { .. })
    ));
}

#[test]
fn a_note_does_not_interrupt_anyone() {
    assert!(Notification::for_message(&row("note", false)).is_none());
}

#[test]
fn an_unsupported_kind_does_not_interrupt_anyone_either() {
    // An unrecognized kind is sender-chosen text this version does not
    // understand. Notifying on it would let a sender decide when someone's
    // screen lights up.
    assert!(Notification::for_message(&row("unsupported", false)).is_none());
    assert!(Notification::for_message(&row("../../etc/passwd", false)).is_none());
}

#[test]
fn a_tamper_halt_is_urgent_and_names_no_attacker_supplied_reason() {
    let notification = Notification::TamperDetected {
        channel_local_name: "Team channel".to_owned(),
    };

    assert!(notification.is_urgent());

    let rendered = notification.render();
    assert!(rendered.contains("Team channel"));
    assert!(rendered.contains("HALTED"));
}

#[test]
fn a_prompt_request_is_urgent_because_it_is_blocking_on_a_human() {
    let notification = Notification::NewPromptRequest {
        sender_local_name: "Alice".to_owned(),
        channel_local_name: "Team channel".to_owned(),
    };

    assert!(notification.is_urgent());
    assert!(notification.render().contains("waiting for your approval"));
}

#[test]
fn an_ordinary_question_is_not_urgent() {
    let notification = Notification::NewQuestion {
        sender_local_name: "Alice".to_owned(),
        channel_local_name: "Team channel".to_owned(),
    };

    assert!(!notification.is_urgent());
}

#[test]
fn every_rendered_notification_names_its_channel_and_nothing_else_remote() {
    let notifications = [
        Notification::NewQuestion {
            sender_local_name: "Alice".to_owned(),
            channel_local_name: "Team".to_owned(),
        },
        Notification::NewPromptRequest {
            sender_local_name: "Alice".to_owned(),
            channel_local_name: "Team".to_owned(),
        },
        Notification::NewTaskRequest {
            sender_local_name: "Alice".to_owned(),
            channel_local_name: "Team".to_owned(),
        },
        Notification::NewResult {
            sender_local_name: "Alice".to_owned(),
            channel_local_name: "Team".to_owned(),
        },
        Notification::JoinAwaitingApproval {
            channel_local_name: "Team".to_owned(),
            waiting: 1,
        },
        Notification::FailedDelivery {
            channel_local_name: "Team".to_owned(),
        },
        Notification::MembershipChanged {
            channel_local_name: "Team".to_owned(),
            roster_epoch: 4,
        },
        Notification::TamperDetected {
            channel_local_name: "Team".to_owned(),
        },
    ];

    // Section 23.3 lists eight notification reasons and this is all of them.
    assert_eq!(notifications.len(), 8);

    for notification in &notifications {
        let rendered = notification.render();
        assert!(
            rendered.contains("Team"),
            "`{rendered}` should name its channel"
        );
        assert!(!rendered.is_empty());
    }
}

#[test]
fn every_notification_has_a_distinct_fixed_kind() {
    // The ledger deduplicates on this. Two variants sharing a tag would make
    // a notice about one thing suppress a notice about another.
    let all = [
        Notification::NewQuestion {
            sender_local_name: "Alice".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewPromptRequest {
            sender_local_name: "Alice".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewTaskRequest {
            sender_local_name: "Alice".into(),
            channel_local_name: "Team".into(),
        },
        Notification::JoinAwaitingApproval {
            channel_local_name: "Team".into(),
            waiting: 1,
        },
        Notification::FailedDelivery {
            channel_local_name: "Team".into(),
        },
        Notification::MembershipChanged {
            channel_local_name: "Team".into(),
            roster_epoch: 2,
        },
        Notification::TamperDetected {
            channel_local_name: "Team".into(),
        },
    ];

    let kinds: std::collections::BTreeSet<&str> = all.iter().map(Notification::kind).collect();

    assert_eq!(kinds.len(), all.len(), "two variants share a tag");

    // The tag is also the serialized discriminant, so the stored key and the
    // JSON a pane emits cannot drift apart.
    for notification in &all {
        let rendered = serde_json::to_value(notification).expect("a notification serializes");
        assert_eq!(rendered["kind"], notification.kind());
    }
}

#[test]
fn a_burst_of_ordinary_arrivals_becomes_one_notice() {
    let arrivals = vec![
        Notification::NewQuestion {
            sender_local_name: "Alice".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewTaskRequest {
            sender_local_name: "Bob".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewQuestion {
            sender_local_name: "Carol".into(),
            channel_local_name: "Team".into(),
        },
    ];

    let raised = coalesce(arrivals);

    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].text, "3 new remote requests");
    assert_eq!(raised[0].covers, 3);
    assert!(!raised[0].urgent);

    // Fixed local wording and a count. No sender name, no channel name, and
    // no path at all from anything a sender wrote.
    for sender in ["Alice", "Bob", "Carol"] {
        assert!(!raised[0].text.contains(sender));
    }
}

#[test]
fn one_arrival_is_still_named() {
    // Collapsing a single arrival into "1 new remote request" would throw
    // away who and where for no gain.
    let raised = coalesce(vec![Notification::NewQuestion {
        sender_local_name: "Alice".into(),
        channel_local_name: "Team".into(),
    }]);

    assert_eq!(raised.len(), 1);
    assert_eq!(raised[0].covers, 1);
    assert!(raised[0].text.contains("Alice"));
}

#[test]
fn urgent_notices_are_never_folded_into_a_count() {
    // A tamper halt and a prompt request each name one thing a person has to
    // act on. "4 new remote requests" would lose what made them urgent.
    let raised = coalesce(vec![
        Notification::TamperDetected {
            channel_local_name: "Team".into(),
        },
        Notification::NewPromptRequest {
            sender_local_name: "Alice".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewQuestion {
            sender_local_name: "Bob".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewTaskRequest {
            sender_local_name: "Carol".into(),
            channel_local_name: "Team".into(),
        },
    ]);

    let urgent: Vec<&Coalesced> = raised.iter().filter(|notice| notice.urgent).collect();
    assert_eq!(urgent.len(), 2);
    assert!(urgent.iter().any(|notice| notice.text.contains("HALTED")));
    assert!(urgent.iter().any(|notice| notice.text.contains("Alice")));

    let ordinary: Vec<&Coalesced> = raised.iter().filter(|notice| !notice.urgent).collect();
    assert_eq!(ordinary.len(), 1);
    assert_eq!(ordinary[0].text, "2 new remote requests");
}

#[test]
fn nothing_new_raises_nothing() {
    assert!(coalesce(Vec::new()).is_empty());
}

#[test]
fn a_coalesced_notice_carries_no_body_or_attachment_name() {
    // A notification reaches a human out of context and may be mirrored to a
    // phone. Serializing it and finding nothing but fixed wording, a flag
    // and a count is the same argument the row type makes.
    let raised = coalesce(vec![
        Notification::NewQuestion {
            sender_local_name: "Alice".into(),
            channel_local_name: "Team".into(),
        },
        Notification::NewQuestion {
            sender_local_name: "Bob".into(),
            channel_local_name: "Team".into(),
        },
    ]);

    let rendered = serde_json::to_value(&raised[0]).expect("a notice serializes");
    let object = rendered.as_object().expect("a notice is an object");

    // Sorted, because `serde_json`'s object preserves insertion order only
    // with the `preserve_order` feature and this must not depend on it.
    let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
    keys.sort_unstable();
    assert_eq!(keys, vec!["covers", "text", "urgent"]);
}

#[test]
fn a_task_result_tells_the_requester_without_saying_how_it_went() {
    // What a requester is waiting on. Whether it succeeded is in the body,
    // which stays quarantined, so the notice says only that one arrived.
    let raised = Notification::for_message(&row("result", false)).expect("a result is notified");
    assert!(matches!(raised, Notification::NewResult { .. }));
    assert!(!raised.is_urgent());
    assert_eq!(raised.kind(), "new_result");

    for quiet in ["task_accept", "task_decline", "progress"] {
        assert!(
            Notification::for_message(&row(quiet, false)).is_none(),
            "{quiet}"
        );
    }
}
