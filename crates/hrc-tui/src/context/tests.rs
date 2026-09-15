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

fn app() -> ContextApp {
    ContextApp::new(
        vec![
            ContextDraft {
                package_id: "ctx-review".into(),
                digest: "abcdef0123456789abcdef0123456789".into(),
            },
            ContextDraft {
                package_id: "ctx-other".into(),
                digest: "99887766554433221100aabbccddeeff".into(),
            },
        ],
        vec!["principal-alice".into(), "principal-bob".into()],
    )
}

fn clean() -> ContextPreview {
    ContextPreview {
        items: vec![("file_excerpt".into(), 512)],
        total_bytes: 512,
        secrets: Vec::new(),
        excluded: Vec::new(),
        sendable: true,
    }
}

#[test]
fn nothing_can_be_sent_before_the_preview_is_seen() {
    // The preview is the disclosure decision. Sending without it would make
    // the trusted screen a confirmation dialog over contents nobody read.
    let mut app = app();

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    assert!(app.proposed().is_none());
    assert!(
        app.status().contains("Enter first"),
        "the screen should say what is missing: {}",
        app.status()
    );
}

#[test]
fn enter_asks_the_daemon_about_the_current_selection() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(ContextOutcome::Preview {
            package_id: "ctx-other".into(),
            recipient: "principal-alice".into(),
        })
    );
}

#[test]
fn a_package_with_secret_findings_cannot_be_confirmed() {
    // A finding is the entire reason preview exists. The daemon refuses too,
    // and that refusal is the one that counts — this one is so the human is
    // told why rather than watching a confirmation fail.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(ContextPreview {
        items: vec![("file_excerpt".into(), 64)],
        total_bytes: 64,
        secrets: vec!["item 0: aws_secret_key".into()],
        excluded: Vec::new(),
        sendable: false,
    });

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    assert!(
        app.proposed().is_none(),
        "a package with findings must not reach a confirmation"
    );
    assert!(app.status().contains("cannot be sent"), "{}", app.status());
}

#[test]
fn an_excluded_path_blocks_it_too() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(ContextPreview {
        items: vec![("file_excerpt".into(), 64)],
        total_bytes: 64,
        secrets: Vec::new(),
        excluded: vec![".env".into()],
        sendable: false,
    });

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    assert!(app.proposed().is_none());
}

#[test]
fn the_confirmation_names_the_digest_and_the_recipient() {
    // What is being authorized is a specific set of bytes for a specific
    // person. A package name alone could be re-drafted with different
    // contents between the preview and the send.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());
    app.on_key(control(KeyCode::Char('s')));

    let prompt = app.proposed().expect("a proposal");
    assert!(prompt.contains("principal-alice"), "{prompt}");
    assert!(prompt.contains("abcdef0123456789"), "{prompt}");
    assert!(prompt.contains("512"), "{prompt}");
}

#[test]
fn moving_the_selection_discards_the_preview() {
    // A preview describes one package for one recipient. Leaving it on screen
    // beside a different selection invites a decision about the wrong bytes.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());
    assert!(app.preview().is_some());

    app.on_key(key(KeyCode::Down));
    assert!(app.preview().is_none());

    assert!(app.on_key(control(KeyCode::Char('s'))).is_none());
    assert!(
        app.proposed().is_none(),
        "sending after moving must require a fresh preview"
    );
}

#[test]
fn changing_the_recipient_discards_the_preview() {
    // The authorization binds to the recipient, so a preview taken for one
    // person says nothing about another.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());

    app.on_key(key(KeyCode::Tab));
    assert!(app.preview().is_none());
}

#[test]
fn a_confirmed_send_names_what_was_previewed() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());
    app.on_key(control(KeyCode::Char('s')));

    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(ContextOutcome::Send {
            package_id: "ctx-review".into(),
            recipient: "principal-alice".into(),
        })
    );
}

#[test]
fn anything_other_than_yes_cancels() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());
    app.on_key(control(KeyCode::Char('s')));

    assert!(app.on_key(key(KeyCode::Char('n'))).is_none());
    assert!(app.proposed().is_none());
}

#[test]
fn escape_closes_the_preview_before_the_screen() {
    // Leaving takes a second deliberate press, like the message screen.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());

    assert!(app.on_key(key(KeyCode::Esc)).is_none());
    assert!(app.preview().is_none());
    assert_eq!(app.on_key(key(KeyCode::Esc)), Some(ContextOutcome::Quit));
}

#[test]
fn a_key_release_neither_previews_nor_confirms() {
    let mut app = app();
    assert!(app.on_key(release(KeyCode::Enter)).is_none());

    app.on_key(key(KeyCode::Enter));
    app.show_preview(clean());
    app.on_key(control(KeyCode::Char('s')));

    assert!(app.on_key(release(KeyCode::Char('s'))).is_none());
    assert!(
        app.proposed().is_some(),
        "a release must not answer the confirmation"
    );
}
