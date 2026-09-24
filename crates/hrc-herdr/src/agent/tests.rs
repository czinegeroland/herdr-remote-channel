use super::*;

fn agents() -> Vec<LocalAgent> {
    vec![
        LocalAgent::new("pane-7f3a", "Reviewer"),
        LocalAgent::new("pane-91bc", "Builder"),
    ]
}

#[test]
fn nothing_is_selected_until_the_human_chooses() {
    // Section 19.2: the trusted screen opens with no decision selected.
    let selection = Selection::new(agents());

    assert!(selection.selected().is_none());
    assert!(!selection.ready_for_delivery());
}

#[test]
fn choosing_an_agent_makes_delivery_available() {
    let mut selection = Selection::new(agents());

    assert!(selection.select(1));
    assert_eq!(selection.selected().map(LocalAgent::label), Some("Builder"));
    assert!(selection.ready_for_delivery());
}

#[test]
fn selecting_an_agent_that_is_not_there_changes_nothing() {
    let mut selection = Selection::new(agents());

    assert!(!selection.select(9));
    assert!(selection.selected().is_none());
}

#[test]
fn clearing_the_selection_withdraws_delivery() {
    let mut selection = Selection::new(agents());
    selection.select(0);

    selection.clear();

    assert!(!selection.ready_for_delivery());
}

#[test]
fn an_empty_installation_can_never_deliver_a_prompt() {
    let selection = Selection::new(Vec::new());

    assert!(!selection.ready_for_delivery());
}

#[test]
fn only_the_human_chosen_label_is_disclosable() {
    // PRD section 4 constraint 8 and section 23.4: remote participants never
    // receive local pane IDs. `LocalAgent` is not `Serialize`, so the pane ID
    // has no path onto the wire; this asserts the one conversion that exists
    // does not carry it either.
    let agent = LocalAgent::new("pane-7f3a", "Reviewer");

    assert_eq!(agent.disclosable_label(), "Reviewer");
    assert!(!agent.disclosable_label().contains("pane"));
    assert_eq!(agent.local_pane_id(), "pane-7f3a");
}

#[test]
fn a_new_session_is_offered_by_kind_and_only_for_a_well_formed_one() {
    // Decision DEC-111. Kinds come from the local Herdr, but they reach a
    // label and a host request, so a malformed one is not offered.
    let session = LocalAgent::new_session("claude").expect("a well-formed kind");
    assert_eq!(session.starts_new(), Some("claude"));
    assert_eq!(session.label(), "a new claude session");
    assert!(session.is_ready(), "starting it is what the approval does");
    assert!(!session.is_working());

    for bad in ["", "claude code", "claude;rm", "../x", &"k".repeat(33)] {
        assert!(
            LocalAgent::new_session(bad).is_none(),
            "{bad:?} was offered"
        );
    }

    assert_eq!(LocalAgent::new("w1:p1", "reviewer").starts_new(), None);
}
