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

fn app() -> SetupApp {
    SetupApp::new(vec![
        SetupStep::CreateChannel,
        SetupStep::CreateInvite,
        SetupStep::Join,
    ])
}

fn type_into(app: &mut SetupApp, text: &str) {
    app.on_key(key(KeyCode::Enter));
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
    app.on_key(key(KeyCode::Enter));
}

#[test]
fn a_channel_is_created_on_the_repository_that_was_typed() {
    let mut app = app();
    type_into(&mut app, "owner/repo");

    let outcome = app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        outcome,
        Some(SetupOutcome::CreateChannel {
            repo: "owner/repo".into()
        })
    );
}

#[test]
fn the_confirmation_names_the_repository_it_would_publish_to() {
    // Creating a channel publishes a genesis object. Naming the repository is
    // the difference between confirming a decision and confirming a keypress.
    let mut app = app();
    type_into(&mut app, "owner/repo");

    let prompt = app.proposed().expect("a proposal");
    assert!(prompt.contains("owner/repo"), "{prompt}");
}

#[test]
fn an_invite_code_is_never_echoed_back() {
    // An invite code is a bearer secret: anyone holding it can present a join
    // request. It gets typed in front of whoever can see the screen.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected(), Some(SetupStep::Join));

    app.on_key(key(KeyCode::Enter));
    for character in "secret-code".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }

    let shown = app.displayed_input();
    assert!(
        !shown.contains("secret"),
        "the code must not be displayed: {shown}"
    );
    assert_eq!(shown, "*".repeat("secret-code".len()));

    app.on_key(key(KeyCode::Enter));
    let prompt = app.proposed().expect("a proposal");
    assert!(
        !prompt.contains("secret-code"),
        "the confirmation must not echo it either: {prompt}"
    );
}

#[test]
fn the_code_still_reaches_the_caller_intact() {
    // Masking is a display decision. The value itself has to survive it.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    type_into(&mut app, "secret-code");

    let outcome = app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        outcome,
        Some(SetupOutcome::Join {
            invite_code: "secret-code".into()
        })
    );
}

#[test]
fn nothing_runs_without_a_value() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Enter));

    assert!(app.proposed().is_none());
    assert!(app.status().contains("Nothing typed"), "{}", app.status());
}

#[test]
fn anything_other_than_yes_cancels() {
    let mut app = app();
    type_into(&mut app, "owner/repo");

    assert!(app.on_key(key(KeyCode::Char('n'))).is_none());
    assert!(app.proposed().is_none());
}

#[test]
fn escape_while_typing_goes_back_and_clears_the_value() {
    // Unlike a half-written message, a half-typed invite code is something
    // to drop rather than keep: leaving it in the buffer would carry a
    // secret into the next step the person chose.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Enter));
    for character in "secret".chars() {
        app.on_key(key(KeyCode::Char(character)));
    }

    app.on_key(key(KeyCode::Esc));
    assert_eq!(app.focus(), SetupFocus::Menu);
    assert_eq!(app.displayed_input(), "");
}

#[test]
fn a_key_release_neither_types_nor_confirms() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('o')));
    app.on_key(release(KeyCode::Char('o')));

    assert_eq!(app.displayed_input(), "o");
}

#[test]
fn only_the_offered_steps_appear() {
    // The caller filters by what makes sense: offering "invite someone" to an
    // installation with no channel is offering a step that can only fail.
    let app = SetupApp::new(vec![SetupStep::Join]);
    assert_eq!(app.steps(), &[SetupStep::Join]);
    assert_eq!(app.selected(), Some(SetupStep::Join));
}
