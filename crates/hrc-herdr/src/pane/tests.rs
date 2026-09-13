use super::*;

#[test]
fn every_pane_round_trips_through_its_name() {
    for pane in Pane::ALL {
        assert_eq!(Pane::parse(pane.as_str()), Some(pane));
    }
}

#[test]
fn the_prd_names_the_inbox_pane() {
    // PRD section 13.2 lists `hrc herdr pane inbox`.
    assert_eq!(Pane::parse("inbox"), Some(Pane::Inbox));
}

#[test]
fn an_unknown_pane_is_refused_rather_than_guessed_at() {
    assert_eq!(Pane::parse("Inbox"), None);
    assert_eq!(Pane::parse(""), None);
    assert_eq!(Pane::parse("approvals"), None);
}
