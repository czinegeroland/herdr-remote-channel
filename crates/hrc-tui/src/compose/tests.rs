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

fn control(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::CONTROL,
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

fn app() -> ComposeApp {
    ComposeApp::new(vec![
        Recipient {
            display_name: None,
            principal_id: "principal-alice".into(),
            active: true,
            in_reply_to: None,
            answering: None,
        },
        Recipient {
            display_name: None,
            principal_id: "principal-bob".into(),
            active: true,
            in_reply_to: None,
            answering: None,
        },
        Recipient {
            display_name: None,
            principal_id: "principal-gone".into(),
            active: false,
            in_reply_to: None,
            answering: None,
        },
    ])
}

fn write(app: &mut ComposeApp, text: &str) {
    app.on_key(key(KeyCode::Tab));
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

#[test]
fn a_message_is_written_and_sent_to_the_selected_recipient() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    write(&mut app, "hello");

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    let outcome = app.on_key(key(KeyCode::Char('y')));

    assert_eq!(
        outcome,
        Some(ComposeOutcome::Send {
            recipient: "principal-bob".into(),
            kind: ComposeKind::Note,
            text: "hello".into(),
            in_reply_to: None,
        })
    );
}

#[test]
fn the_confirmation_names_the_recipient() {
    // Two principals can differ by a few characters of base64, and this is
    // the last point where a human can notice they picked the wrong one.
    let mut app = app();
    write(&mut app, "hello");
    app.on_key(control(KeyCode::Char('s')));

    let prompt = app.proposed().expect("a proposal");
    assert!(
        prompt.contains("principal-alice"),
        "the prompt should name who it goes to: {prompt}"
    );
}

#[test]
fn an_empty_message_is_not_sent() {
    // An empty send is almost always a mis-keyed one, and it costs the
    // recipient a decision either way.
    let mut app = app();
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char(' ')));

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    assert!(
        app.proposed().is_none(),
        "an empty message should not reach a confirmation"
    );
}

#[test]
fn an_inactive_member_cannot_be_written_to() {
    // The roster says they are gone. Publishing to them would be a message
    // nobody can read, recorded in a history nobody can edit.
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    app.on_key(key(KeyCode::Down));
    write(&mut app, "hello");

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    assert!(app.proposed().is_none());
    assert!(
        app.status().contains("no longer active"),
        "the screen should say why: {}",
        app.status()
    );
}

#[test]
fn anything_other_than_yes_cancels_without_losing_the_message() {
    // Cancelling a send must not throw away what was typed; that is how
    // people learn to compose somewhere else and paste it in.
    let mut app = app();
    write(&mut app, "hello");
    app.on_key(control(KeyCode::Char('s')));

    assert!(app.on_key(key(KeyCode::Char('n'))).is_none());
    assert_eq!(app.body(), "hello");
}

#[test]
fn escape_while_writing_returns_to_the_recipients_rather_than_leaving() {
    let mut app = app();
    write(&mut app, "hello");

    assert!(app.on_key(key(KeyCode::Esc)).is_none());
    assert_eq!(app.focus(), ComposeFocus::Recipient);
    assert_eq!(app.body(), "hello", "the draft should survive");
}

#[test]
fn the_kind_can_be_changed_before_sending() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('t')));
    assert_eq!(app.kind(), ComposeKind::Question);

    write(&mut app, "does it work?");
    app.on_key(control(KeyCode::Char('s')));
    let outcome = app.on_key(key(KeyCode::Char('y')));

    assert_eq!(
        outcome,
        Some(ComposeOutcome::Send {
            recipient: "principal-alice".into(),
            kind: ComposeKind::Question,
            text: "does it work?".into(),
            in_reply_to: None,
        })
    );
}

#[test]
fn a_key_release_neither_types_nor_confirms() {
    // On Windows every keystroke arrives twice. Typing would double each
    // character, and the send confirmation would be answered by letting go
    // of the key that proposed it.
    let mut app = app();
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('h')));
    app.on_key(release(KeyCode::Char('h')));

    assert_eq!(app.body(), "h", "a release must not insert a character");

    app.on_key(control(KeyCode::Char('s')));
    assert!(app.on_key(release(KeyCode::Char('s'))).is_none());
    assert!(
        app.proposed().is_some(),
        "a release must not answer the confirmation"
    );
}

#[test]
fn a_reply_carries_the_thread_it_answers() {
    // A reply is the same act as a note — text to one person — so it is a
    // target in the same list rather than a screen of its own. What differs
    // is that the thread comes from local state: a sender that could choose
    // its own thread could attach a reply to any conversation.
    let mut app = ComposeApp::new(vec![Recipient {
        display_name: None,
        principal_id: "principal-alice".into(),
        active: true,
        in_reply_to: Some("01ARYZ6S41000000000000000A".into()),
        answering: Some("question".into()),
    }]);

    write(&mut app, "yes it does");
    app.on_key(control(KeyCode::Char('s')));

    let prompt = app.proposed().expect("a proposal");
    assert!(
        prompt.contains("Reply to"),
        "the confirmation should say it joins a conversation: {prompt}"
    );
    assert!(prompt.contains("question"), "{prompt}");

    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(ComposeOutcome::Send {
            recipient: "principal-alice".into(),
            kind: ComposeKind::Note,
            text: "yes it does".into(),
            in_reply_to: Some("01ARYZ6S41000000000000000A".into()),
        })
    );
}
