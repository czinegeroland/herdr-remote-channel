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

    // Section 23.3 lists seven notification reasons and this is all of them.
    assert_eq!(notifications.len(), 7);

    for notification in &notifications {
        let rendered = notification.render();
        assert!(
            rendered.contains("Team"),
            "`{rendered}` should name its channel"
        );
        assert!(!rendered.is_empty());
    }
}
