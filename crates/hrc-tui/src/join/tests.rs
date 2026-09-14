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

fn app() -> JoinApp {
    JoinApp::new(
        vec![
            PendingJoin {
                request_id: "request-1".into(),
                principal_id: "principal-alice".into(),
                device_id: "device-1".into(),
                created_at: "2026-09-13T00:00:00Z".into(),
                safety_phrase: "amber-otter-lantern".into(),
            },
            PendingJoin {
                request_id: "request-2".into(),
                principal_id: "principal-bob".into(),
                device_id: "device-2".into(),
                created_at: "2026-09-13T00:01:00Z".into(),
                safety_phrase: "cobalt-finch-marble".into(),
            },
        ],
        "Team channel",
    )
}

#[test]
fn admitting_takes_two_deliberate_presses() {
    // HRC-CH-007: joins need explicit administrator approval. One key press
    // must never add a member.
    let mut app = app();

    assert!(app.on_key(key(KeyCode::Char('a'))).is_none());
    assert!(app.proposed().is_some());

    let outcome = app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        outcome,
        Some(JoinOutcome::Admit {
            request_id: "request-1".into()
        })
    );
}

#[test]
fn the_confirmation_names_the_phrase_that_was_supposed_to_be_checked() {
    // The safety phrase is the only thing that establishes who is joining.
    // A prompt that asked "are you sure?" would be a question about the
    // administrator's confidence rather than about the evidence, and it is
    // the evidence that decides whether this is the right person.
    let mut app = app();
    app.on_key(key(KeyCode::Char('a')));

    let prompt = &app.proposed().expect("a proposal").prompt;
    assert!(
        prompt.contains("amber-otter-lantern"),
        "the prompt should name the phrase: {prompt}"
    );
    assert!(
        prompt.contains("Team channel"),
        "the prompt should name the channel: {prompt}"
    );
}

#[test]
fn anything_other_than_yes_cancels() {
    // A mistyped key must never admit anyone.
    let mut app = app();
    app.on_key(key(KeyCode::Char('a')));

    assert!(app.on_key(key(KeyCode::Char('n'))).is_none());
    assert!(
        app.proposed().is_none(),
        "the proposal should be gone rather than still armed"
    );
}

#[test]
fn a_key_release_neither_proposes_nor_confirms() {
    // Windows reports a press and a release for one physical key. Counting
    // both would let a single keystroke propose an admission and then
    // confirm it, which is exactly the two-press rule this screen exists to
    // enforce.
    let mut app = app();

    assert!(app.on_key(release(KeyCode::Char('a'))).is_none());
    assert!(
        app.proposed().is_none(),
        "a release must not propose anything"
    );

    app.on_key(key(KeyCode::Char('a')));
    assert!(
        app.on_key(release(KeyCode::Char('a'))).is_none(),
        "a release must not confirm the proposal"
    );
    assert!(
        app.proposed().is_some(),
        "the proposal should still be waiting for a deliberate answer"
    );
}

#[test]
fn moving_the_selection_cancels_a_proposal() {
    // Otherwise a proposal armed against one request could be confirmed
    // after the selection moved, and the wrong person would be admitted.
    let mut app = app();
    app.on_key(key(KeyCode::Char('a')));
    assert!(app.proposed().is_some());

    app.on_key(key(KeyCode::Down));
    assert!(app.proposed().is_none());

    let outcome = app.on_key(key(KeyCode::Char('y')));
    assert!(
        outcome.is_none(),
        "confirming after moving must not admit anyone"
    );
}

#[test]
fn refusing_is_reported_separately_from_admitting() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('r')));
    let outcome = app.on_key(key(KeyCode::Char('y')));

    assert_eq!(
        outcome,
        Some(JoinOutcome::Refuse {
            request_id: "request-1".into()
        })
    );
}

#[test]
fn leaving_decides_nothing() {
    let mut app = app();
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Some(JoinOutcome::Quit));
}
