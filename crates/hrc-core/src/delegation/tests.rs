//! Delegation tracking tests.
//!
//! The lifecycle rules are tested in the protocol crate. What is tested here
//! is who is allowed to move a task, which is the part an attacker would
//! rather not be bound by: accepting on someone else's behalf, or declaring
//! one's own work reviewed and done.

use super::*;

fn delegation() -> Delegation {
    Delegation::open("01ARZ3NDEKTSV4RRFFQ69G5FAV", "alice", "bob")
}

#[test]
fn a_task_runs_through_its_lifecycle() {
    let mut task = delegation();

    task.apply("bob", DelegationState::Accepted).unwrap();
    task.apply("bob", DelegationState::InProgress).unwrap();
    task.apply("bob", DelegationState::NeedsInput).unwrap();
    task.apply("bob", DelegationState::InProgress).unwrap();
    task.apply("bob", DelegationState::ResultPendingReview)
        .unwrap();
    task.apply("alice", DelegationState::Completed).unwrap();

    assert_eq!(task.state, DelegationState::Completed);
    assert!(task.is_finished());
}

#[test]
fn an_outsider_cannot_report_anything() {
    let mut task = delegation();

    let error = task.apply("carol", DelegationState::Accepted).unwrap_err();

    assert!(matches!(error, CoreError::NotADelegationParty { .. }));
    assert_eq!(task.state, DelegationState::Requested);
}

#[test]
fn the_requester_cannot_accept_on_the_assignees_behalf() {
    let mut task = delegation();

    let error = task.apply("alice", DelegationState::Accepted).unwrap_err();

    assert!(matches!(error, CoreError::WrongDelegationParty { .. }));
    assert_eq!(task.state, DelegationState::Requested);
}

#[test]
fn the_assignee_cannot_declare_its_own_work_complete() {
    // Completion is the requester's judgement after reviewing a result.
    // Otherwise "done" would mean only that the assignee said so.
    let mut task = delegation();
    task.apply("bob", DelegationState::Accepted).unwrap();
    task.apply("bob", DelegationState::ResultPendingReview)
        .unwrap();

    let error = task.apply("bob", DelegationState::Completed).unwrap_err();

    assert!(matches!(error, CoreError::WrongDelegationParty { .. }));
    assert_eq!(task.state, DelegationState::ResultPendingReview);
}

#[test]
fn only_the_requester_withdraws_the_request() {
    let mut task = delegation();

    assert!(task.apply("bob", DelegationState::Cancelled).is_err());
    task.apply("alice", DelegationState::Cancelled).unwrap();
    assert!(task.is_finished());
}

#[test]
fn expiry_is_not_something_a_peer_can_report() {
    // Expiry is what happens when nobody says anything, so a peer claiming
    // it would be reporting on a clock the other side cannot check.
    for reporter in ["alice", "bob"] {
        let mut task = delegation();
        assert!(
            task.apply(reporter, DelegationState::Expired).is_err(),
            "{reporter} was allowed to declare expiry"
        );
    }
}

#[test]
fn a_task_cannot_be_re_requested() {
    let mut task = delegation();
    assert!(task.apply("alice", DelegationState::Requested).is_err());
    assert!(task.apply("bob", DelegationState::Requested).is_err());
}

#[test]
fn work_cannot_start_before_the_task_is_accepted() {
    let mut task = delegation();

    let error = task.apply("bob", DelegationState::InProgress).unwrap_err();

    assert!(matches!(
        error,
        CoreError::IllegalDelegationTransition {
            from: "requested",
            to: "in_progress",
            ..
        }
    ));
}

#[test]
fn a_declined_task_stays_declined() {
    let mut task = delegation();
    task.apply("bob", DelegationState::Declined).unwrap();

    for state in DelegationState::ALL {
        for reporter in ["alice", "bob"] {
            assert!(
                task.clone().apply(reporter, state).is_err(),
                "{reporter} reopened a declined task as {state:?}"
            );
        }
    }
}

#[test]
fn a_completed_task_cannot_be_reopened_by_either_side() {
    let mut task = delegation();
    task.apply("bob", DelegationState::Accepted).unwrap();
    task.apply("bob", DelegationState::ResultPendingReview)
        .unwrap();
    task.apply("alice", DelegationState::Completed).unwrap();

    for state in DelegationState::ALL {
        for reporter in ["alice", "bob"] {
            assert!(task.clone().apply(reporter, state).is_err());
        }
    }
}

#[test]
fn a_rejected_transition_leaves_the_state_untouched() {
    // A failed report must not half-apply, or a peer could walk a task
    // forward by sending transitions it is not entitled to make.
    let mut task = delegation();
    task.apply("bob", DelegationState::Accepted).unwrap();

    let before = task.clone();
    assert!(task.apply("alice", DelegationState::InProgress).is_err());
    assert!(task.apply("carol", DelegationState::Failed).is_err());
    assert!(task.apply("bob", DelegationState::Completed).is_err());

    assert_eq!(task, before);
}

#[test]
fn each_principal_is_on_exactly_one_side() {
    let task = delegation();

    assert_eq!(task.party_of("alice"), Some(Party::Requester));
    assert_eq!(task.party_of("bob"), Some(Party::Assignee));
    assert_eq!(task.party_of("carol"), None);
}
