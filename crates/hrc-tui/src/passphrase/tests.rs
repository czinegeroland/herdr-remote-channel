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

fn typed(app: &mut PassphraseApp, text: &str) {
    for character in text.chars() {
        app.on_key(key(KeyCode::Char(character)));
    }
}

#[test]
fn the_entry_is_never_displayed() {
    let mut app = PassphraseApp::new("send a message");
    typed(&mut app, "correct horse");

    let shown = app.masked();
    assert!(!shown.contains("horse"), "{shown}");
    assert_eq!(shown, "*".repeat("correct horse".len()));
}

#[test]
fn the_entry_still_reaches_the_caller_intact() {
    let mut app = PassphraseApp::new("send a message");
    typed(&mut app, "correct horse");

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(PassphraseOutcome::Unlock("correct horse".into()))
    );
}

#[test]
fn the_prompt_says_what_it_is_for() {
    // A prompt that only says "passphrase:" asks someone to type a secret
    // without saying what happens next, which is the shape of every
    // credential-phishing screen ever built.
    let app = PassphraseApp::new("admit a member");
    assert_eq!(app.reason(), "admit a member");
}

#[test]
fn an_empty_entry_does_not_unlock() {
    let mut app = PassphraseApp::new("send a message");
    assert!(app.on_key(key(KeyCode::Enter)).is_none());
}

#[test]
fn escape_leaves_without_unlocking() {
    let mut app = PassphraseApp::new("send a message");
    typed(&mut app, "secret");
    assert_eq!(app.on_key(key(KeyCode::Esc)), Some(PassphraseOutcome::Quit));
}

#[test]
fn a_key_release_does_not_type() {
    // On Windows every keystroke arrives twice, which would double every
    // character of a passphrase and make it impossible to enter one.
    let mut app = PassphraseApp::new("send a message");
    app.on_key(key(KeyCode::Char('a')));
    app.on_key(release(KeyCode::Char('a')));

    assert_eq!(app.masked(), "*");
}

#[test]
fn backspace_removes_one_character() {
    let mut app = PassphraseApp::new("send a message");
    typed(&mut app, "abc");
    app.on_key(key(KeyCode::Backspace));

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(PassphraseOutcome::Unlock("ab".into()))
    );
}
