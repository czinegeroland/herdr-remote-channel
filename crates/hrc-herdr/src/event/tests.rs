use super::*;

use crate::pane::Pane;

#[test]
fn a_startup_event_refreshes_the_sidebar() {
    let event = Event::parse(r#"{"event":"started"}"#).expect("a started event parses");

    assert_eq!(event, Event::Started);
    assert_eq!(event.reaction(), Reaction::RefreshStatus);
}

#[test]
fn opening_the_inbox_pane_redraws_it() {
    let event = Event::parse(r#"{"event":"pane_opened","params":{"pane":"inbox"}}"#)
        .expect("a pane event parses");

    assert_eq!(
        event.reaction(),
        Reaction::RefreshPane { pane: Pane::Inbox }
    );
}

#[test]
fn a_pane_this_plugin_does_not_own_is_left_alone() {
    let event = Event::parse(r#"{"event":"pane_opened","params":{"pane":"someone_elses"}}"#)
        .expect("a pane event parses");

    assert_eq!(event.reaction(), Reaction::Ignore);
}

#[test]
fn a_tick_refreshes_and_shutdown_does_nothing() {
    assert_eq!(Event::Tick.reaction(), Reaction::RefreshStatus);
    assert_eq!(Event::Stopping.reaction(), Reaction::Ignore);
}

#[test]
fn an_unknown_event_is_an_error_rather_than_a_silent_no_op() {
    let error =
        Event::parse(r#"{"event":"approve_everything"}"#).expect_err("an unknown event is refused");

    assert!(!error.is_empty());
}

#[test]
fn an_unexpected_field_is_refused_rather_than_ignored() {
    // A dropped parameter is the worst outcome on any boundary: the caller
    // reasonably concludes it was honored.
    Event::parse(r#"{"event":"pane_opened","params":{"pane":"inbox","approve":true}}"#)
        .expect_err("an unknown field is refused");
}

#[test]
fn malformed_input_does_not_panic() {
    Event::parse("").expect_err("empty input is refused");
    Event::parse("not json").expect_err("garbage is refused");
    Event::parse("[]").expect_err("the wrong shape is refused");
}

#[test]
fn no_reaction_can_approve_deliver_or_reveal_anything() {
    // The whole point of the type. If a variant is ever added that could,
    // this assertion is where the reviewer is meant to stop and think.
    for event in [
        Event::Started,
        Event::Tick,
        Event::Stopping,
        Event::PaneOpened {
            pane: "inbox".to_owned(),
        },
    ] {
        let rendered = serde_json::to_string(&event.reaction()).expect("a reaction serializes");

        for forbidden in ["approve", "deliver", "body", "authorization"] {
            assert!(
                !rendered.contains(forbidden),
                "`{rendered}` must not mention `{forbidden}`"
            );
        }
    }
}
