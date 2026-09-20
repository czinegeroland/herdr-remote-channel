use super::*;
use crossterm::event::KeyEventState;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn release(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    }
}

fn device(id: &str, active: bool) -> MemberDevice {
    MemberDevice {
        device_id: id.into(),
        active,
    }
}

fn app() -> MembersApp {
    MembersApp::new(vec![
        Member {
            display_name: None,
            principal_id: "principal-me".into(),
            administrator: true,
            active: true,
            devices: vec![device("device-mine", true)],
            is_local: true,
        },
        Member {
            display_name: None,
            principal_id: "principal-bob".into(),
            administrator: false,
            active: true,
            devices: vec![device("device-b1", true), device("device-b2", true)],
            is_local: false,
        },
        Member {
            display_name: None,
            principal_id: "principal-solo".into(),
            administrator: false,
            active: true,
            devices: vec![device("device-only", true)],
            is_local: false,
        },
    ])
}

#[test]
fn removing_yourself_is_refused() {
    // Removing yourself publishes a control entry revoking the authority that
    // signed it. The channel carries on with no administrator who can act,
    // and nothing in this screen could undo it.
    let mut app = app();

    assert!(app.on_key(key(KeyCode::Char('r'))).is_none());
    assert!(
        app.proposed().is_none(),
        "removing this installation must not reach a confirmation"
    );
    assert!(
        app.status().contains("no way back in"),
        "the screen should say why: {}",
        app.status()
    );
}

#[test]
fn removing_a_member_names_how_many_devices_go_with_them() {
    // Removing a principal takes every device with it, and someone looking at
    // one device may not have that in mind.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('r')));

    let prompt = app.proposed().expect("a proposal");
    assert!(prompt.contains("principal-bob"), "{prompt}");
    assert!(
        prompt.contains('2'),
        "the device count should appear: {prompt}"
    );
    assert!(prompt.contains("cannot be undone"), "{prompt}");

    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(MemberOutcome::Remove {
            principal_id: "principal-bob".into()
        })
    );
}

#[test]
fn revoking_the_last_device_says_what_it_leaves_behind() {
    // A principal with no active device stays in the roster, addressable, and
    // unable to read anything. There are good reasons to do it; it should not
    // be a surprise.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('v')));

    let prompt = app.proposed().expect("a proposal");
    assert!(
        prompt.contains("last active device"),
        "the consequence should be stated: {prompt}"
    );
}

#[test]
fn revoking_one_of_several_devices_does_not_claim_it_is_the_last() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('v')));

    let prompt = app.proposed().expect("a proposal");
    assert!(!prompt.contains("last active device"), "{prompt}");
    assert!(prompt.contains("device-b1"), "{prompt}");
}

#[test]
fn an_already_revoked_device_is_not_revoked_again() {
    let mut app = MembersApp::new(vec![Member {
        display_name: None,
        principal_id: "principal-bob".into(),
        administrator: false,
        active: true,
        devices: vec![device("device-gone", false)],
        is_local: false,
    }]);

    app.on_key(key(KeyCode::Tab));
    assert!(app.on_key(key(KeyCode::Char('v'))).is_none());
    assert!(app.proposed().is_none());
    assert!(
        app.status().contains("already been revoked"),
        "{}",
        app.status()
    );
}

#[test]
fn an_already_removed_member_is_not_removed_again() {
    let mut app = MembersApp::new(vec![Member {
        display_name: None,
        principal_id: "principal-gone".into(),
        administrator: false,
        active: false,
        devices: Vec::new(),
        is_local: false,
    }]);

    assert!(app.on_key(key(KeyCode::Char('r'))).is_none());
    assert!(app.proposed().is_none());
}

#[test]
fn moving_the_selection_cancels_a_proposal() {
    // Otherwise a confirmation could land on a member the human was no longer
    // looking at, and this is the screen where that removes the wrong person.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('r')));
    assert!(app.proposed().is_some());

    app.on_key(key(KeyCode::Down));
    assert!(app.proposed().is_none());

    assert!(
        app.on_key(key(KeyCode::Char('y'))).is_none(),
        "confirming after moving must not remove anyone"
    );
}

#[test]
fn anything_other_than_yes_cancels() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Char('r')));

    assert!(app.on_key(key(KeyCode::Char('n'))).is_none());
    assert!(app.proposed().is_none());
}

#[test]
fn a_key_release_neither_proposes_nor_confirms() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));

    assert!(app.on_key(release(KeyCode::Char('r'))).is_none());
    assert!(app.proposed().is_none());

    app.on_key(key(KeyCode::Char('r')));
    assert!(app.on_key(release(KeyCode::Char('r'))).is_none());
    assert!(
        app.proposed().is_some(),
        "a release must not confirm a removal"
    );
}

#[test]
fn a_name_is_typed_and_returned_for_the_selected_member() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected_member().unwrap().principal_id, "principal-bob");

    assert_eq!(app.on_key(key(KeyCode::Char('n'))), None);
    assert_eq!(app.editing(), Some(""));

    for character in "Bob".chars() {
        assert_eq!(app.on_key(key(KeyCode::Char(character))), None);
    }
    assert_eq!(app.editing(), Some("Bob"));

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(MemberOutcome::Name {
            principal_id: "principal-bob".into(),
            display_name: Some("Bob".into()),
        })
    );
    assert_eq!(app.editing(), None);
}

#[test]
fn an_empty_field_clears_the_name_rather_than_storing_a_blank_one() {
    let mut app = MembersApp::new(vec![Member {
        display_name: Some("Bob".into()),
        principal_id: "principal-bob".into(),
        administrator: false,
        active: true,
        devices: vec![device("device-b1", true)],
        is_local: false,
    }]);

    app.on_key(key(KeyCode::Char('n')));

    // Pre-filled with what is on record, so a correction is an edit rather
    // than a retype -- and so emptying the field is a visible act.
    assert_eq!(app.editing(), Some("Bob"));
    for _ in 0..3 {
        app.on_key(key(KeyCode::Backspace));
    }
    assert_eq!(app.editing(), Some(""));

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(MemberOutcome::Name {
            principal_id: "principal-bob".into(),
            display_name: None,
        })
    );
}

#[test]
fn escape_abandons_a_name_without_changing_anything() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('n')));
    app.on_key(key(KeyCode::Char('X')));

    assert_eq!(app.on_key(key(KeyCode::Esc)), None);
    assert_eq!(app.editing(), None);
    assert!(app.status().contains("n: name"));
}

#[test]
fn the_field_stops_at_the_ceiling_the_daemon_enforces() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('n')));

    for _ in 0..(hrc_core::alias::MAX_ALIAS + 10) {
        app.on_key(key(KeyCode::Char('a')));
    }

    // Refused at the keystroke rather than at the daemon: a field that
    // accepted what the daemon rejects would show a person a name and then
    // throw it away.
    assert_eq!(
        app.editing().unwrap().chars().count(),
        hrc_core::alias::MAX_ALIAS
    );
}

#[test]
fn typing_a_name_cannot_reach_a_membership_change() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('n')));

    // `r` and `v` are remove and revoke on this screen. While a name is
    // being typed they are letters, and nothing published may follow from
    // pressing one.
    for character in ['r', 'v', 'q', 'y'] {
        assert_eq!(app.on_key(key(KeyCode::Char(character))), None);
    }

    assert_eq!(app.editing(), Some("rvqy"));
    assert!(app.proposed().is_none());
}

#[test]
fn a_name_field_ignores_key_releases() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('n')));
    app.on_key(release(KeyCode::Char('z')));

    assert_eq!(app.editing(), Some(""));
}
