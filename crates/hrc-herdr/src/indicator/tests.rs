use super::*;

fn sidebar(unread: usize, approvals: usize, halted: usize) -> Sidebar {
    Sidebar {
        unread,
        approvals,
        synced_seconds_ago: Some(12),
        halted,
        unanswered: 0,
        awaiting_answer: 0,
    }
}

#[test]
fn nothing_waiting_shows_nothing() {
    // Not `0 waiting`. An indicator that always says something is one people
    // stop reading, and a stale count left in somebody else's sidebar is
    // worse than no count at all.
    assert_eq!(
        Indicator::from_sidebar(&sidebar(0, 0, 0)),
        Indicator::default()
    );
}

#[test]
fn unread_notes_alone_do_not_claim_an_ambient_surface() {
    // The same judgement section 23.3 makes when it leaves a note off the
    // notification list: a note is not urgent, and interrupting somebody
    // over one is what makes them switch indicators off.
    assert_eq!(
        Indicator::from_sidebar(&sidebar(7, 0, 0)),
        Indicator::default()
    );
}

#[test]
fn a_decision_waiting_is_what_reaches_the_surfaces() {
    let indicator = Indicator::from_sidebar(&sidebar(7, 2, 0));

    assert_eq!(indicator.token.as_deref(), Some("2 waiting"));
    assert_eq!(indicator.title.as_deref(), Some("hrc: 2 waiting"));
}

#[test]
fn a_halt_wins_over_a_count() {
    // Section 26: a tamper halt is sticky and visible, and it is not
    // something a number should be able to push off a one-word surface.
    let indicator = Indicator::from_sidebar(&sidebar(7, 3, 1));

    assert_eq!(indicator.token.as_deref(), Some("HALTED"));
    assert_eq!(indicator.title.as_deref(), Some("hrc: halted"));
}

#[test]
fn a_token_fits_the_key_and_value_shape_the_host_accepts() {
    // Herdr constrains token keys to `^[A-Za-z0-9_-]{1,32}$`. The value is
    // free text, but it lands in somebody else's sidebar beside other
    // plugins, so it stays short by construction rather than by luck.
    assert!(
        crate::host::TOKEN
            .chars()
            .all(|character| character.is_ascii_alphanumeric()
                || character == '_'
                || character == '-')
    );
    assert!(!crate::host::TOKEN.is_empty() && crate::host::TOKEN.len() <= 32);

    for approvals in [1, 10, 999] {
        let indicator = Indicator::from_sidebar(&sidebar(0, approvals, 0));
        assert!(indicator.token.unwrap().len() <= 16);
    }
}

#[test]
fn nothing_an_envelope_carried_can_reach_an_ambient_surface() {
    // By construction: the input is four numbers. This test exists so that
    // widening `Indicator::from_sidebar` to take a row, a sender or a
    // subject has to delete it first.
    let indicator = Indicator::from_sidebar(&sidebar(1, 1, 0));
    let shown = format!("{indicator:?}");

    for forbidden in ["subject", "body", "@", "http"] {
        assert!(!shown.contains(forbidden), "{shown} leaked {forbidden}");
    }
}
