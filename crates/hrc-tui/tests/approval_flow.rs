//! Terminal interaction tests for the trusted approval screen.
//!
//! These drive real key events through the state machine and read the real
//! rendered buffer, which is what `AC-RUST-TUI` asks for. What they are
//! checking is not that the screen looks right but that it cannot be used to
//! approve something by accident, and that a body is never painted before a
//! human asked for it.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};
use hrc_core::gate::AgentView;
use hrc_tui::{App, Focus, Outcome, PendingItem, render};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

const BODY: &str = "the staging credentials rotated on Tuesday";

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn view(sender: &str, kind: &str) -> AgentView {
    AgentView {
        sender_principal: sender.to_owned(),
        sender_local_name: sender.to_owned(),
        kind: kind.to_owned(),
        channel_local_name: "Team channel".into(),
        endpoint_label: "reviewer".into(),
        ciphertext_bytes: 1024,
        plaintext_bytes: BODY.len() as u64,
        created_at: "2026-09-13T00:00:00Z".into(),
        arrival_at: "2026-09-13T00:00:05Z".into(),
        expires_at: None,
        thread_label: "01ARYZ6S41000000000000000A".into(),
        prompt_request: false,
        attachment_count: 0,
        attachment_bytes: 0,
        awaiting_decision: true,
    }
}

fn app() -> App {
    App::new(
        vec![
            PendingItem {
                message_id: "msg-1".into(),
                view: view("Alice", "question"),
                body: BODY.into(),
            },
            PendingItem {
                message_id: "msg-2".into(),
                view: view("Bob", "note"),
                body: "a second message".into(),
            },
        ],
        "reviewer-pane",
    )
}

/// Everything the screen currently shows, as one string.
fn screen(app: &App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(90, 24)).unwrap();
    terminal.draw(|frame| render(frame, app)).unwrap();

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect::<String>()
}

#[test]
fn the_screen_opens_with_nothing_revealed_and_nothing_proposed() {
    // A screen that opened with a body on display, or with an approval
    // preselected, would make the next key press consequential before the
    // human had read anything.
    let app = app();

    assert!(!app.is_revealed());
    assert!(app.proposed().is_none());
    assert_eq!(app.focus(), Focus::List);

    let screen = screen(&app);
    assert!(!screen.contains(BODY), "the body was painted unbidden");
    assert!(screen.contains("HIDDEN"));
}

#[test]
fn the_body_appears_only_after_a_deliberate_reveal() {
    let mut app = app();

    assert!(app.on_key(key(KeyCode::Enter)).is_none());

    assert!(app.is_revealed());
    assert!(screen(&app).contains(BODY));
}

#[test]
fn moving_the_selection_hides_the_body_again() {
    // Otherwise a revealed pane and a moved selection could disagree about
    // which message is on screen, and a decision would be made about the
    // wrong one.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    assert!(app.is_revealed());

    app.on_key(key(KeyCode::Down));

    assert!(!app.is_revealed());
    assert_eq!(app.selected().unwrap().message_id, "msg-2");
    assert!(!screen(&app).contains(BODY));
}

#[test]
fn a_decision_is_not_reachable_before_the_body_is_shown() {
    // Approving something a human has not been shown is the failure this
    // whole screen exists to prevent.
    for decision in ['a', 'i', 'd'] {
        let mut app = app();
        assert!(app.on_key(key(KeyCode::Char(decision))).is_none());
        assert!(
            app.proposed().is_none(),
            "`{decision}` proposed something with the body hidden"
        );
    }
}

#[test]
fn approval_takes_two_deliberate_presses() {
    // PRD section 19.4: manual approval. One key press must never be enough.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));

    assert!(app.on_key(key(KeyCode::Char('a'))).is_none());
    assert_eq!(app.focus(), Focus::Confirm);
    assert!(app.proposed().is_some());

    let outcome = app.on_key(key(KeyCode::Char('y')));

    assert_eq!(
        outcome,
        Some(Outcome::DeliverToAgent {
            message_id: "msg-1".into(),
            agent: "reviewer-pane".into()
        })
    );
}

#[test]
fn the_confirmation_names_what_is_about_to_happen() {
    // "Are you sure?" trains people to say yes. The prompt has to say what
    // they are agreeing to.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));

    let prompt = &app.proposed().unwrap().prompt;
    assert!(prompt.contains("Alice"), "{prompt}");
    assert!(prompt.contains("reviewer-pane"), "{prompt}");

    assert!(screen(&app).contains("CONFIRM"));
}

#[test]
fn enter_does_not_double_as_consent() {
    // Enter is the key people press to dismiss things. If it confirmed, a
    // habitual press would approve remote content.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));

    let outcome = app.on_key(key(KeyCode::Enter));

    assert_eq!(outcome, None);
    assert!(app.proposed().is_none());
    assert!(app.status().contains("Cancelled"));
}

#[test]
fn anything_other_than_y_cancels() {
    for code in [
        KeyCode::Char('n'),
        KeyCode::Esc,
        KeyCode::Char(' '),
        KeyCode::Down,
        KeyCode::Char('a'),
    ] {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Char('a')));

        assert_eq!(app.on_key(key(code)), None, "{code:?} confirmed");
    }
}

#[test]
fn every_decision_in_section_19_2_is_reachable() {
    let cases = [
        (
            'a',
            Outcome::DeliverToAgent {
                message_id: "msg-1".into(),
                agent: "reviewer-pane".into(),
            },
        ),
        (
            'i',
            Outcome::KeepInInbox {
                message_id: "msg-1".into(),
            },
        ),
        (
            'd',
            Outcome::Decline {
                message_id: "msg-1".into(),
            },
        ),
    ];

    for (press, expected) in cases {
        let mut app = app();
        app.on_key(key(KeyCode::Enter));
        app.on_key(key(KeyCode::Char(press)));

        assert_eq!(app.on_key(key(KeyCode::Char('y'))), Some(expected));
    }
}

#[test]
fn the_screen_is_navigable_by_keyboard_alone() {
    // PRD section 27 requires keyboard navigation for interactive approval.
    let mut app = app();

    for (code, expected) in [
        (KeyCode::Down, 1),
        (KeyCode::Down, 1),
        (KeyCode::Up, 0),
        (KeyCode::Char('j'), 1),
        (KeyCode::Char('k'), 0),
    ] {
        app.on_key(key(code));
        assert_eq!(app.selected_index(), expected, "after {code:?}");
    }
}

#[test]
fn the_selection_is_marked_in_text_not_only_in_colour() {
    // PRD section 27: flows must not rely on colour alone.
    let app = app();
    let screen = screen(&app);

    assert!(screen.contains("> Alice"), "no textual selection marker");
    assert!(screen.contains("  Bob"));
}

#[test]
fn the_status_line_always_says_what_the_next_key_does() {
    let mut app = app();
    assert!(app.status().contains("Enter"));

    app.on_key(key(KeyCode::Enter));
    assert!(app.status().contains("deliver"));
    assert!(app.status().contains("decline"));

    app.on_key(key(KeyCode::Char('d')));
    assert!(app.status().contains("[y/N]"));
}

#[test]
fn escape_closes_the_body_before_it_closes_the_screen() {
    // Leaving takes a second, deliberate press, so a habitual escape does
    // not drop the human out of the inbox entirely.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));

    assert_eq!(app.on_key(key(KeyCode::Esc)), None);
    assert!(!app.is_revealed());

    assert_eq!(app.on_key(key(KeyCode::Esc)), Some(Outcome::Quit));
}

#[test]
fn control_c_leaves_from_anywhere_without_deciding() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));

    let interrupt = KeyEvent {
        code: KeyCode::Char('c'),
        modifiers: KeyModifiers::CONTROL,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    };

    assert_eq!(app.on_key(interrupt), Some(Outcome::Quit));
}

#[test]
fn an_empty_inbox_renders_and_decides_nothing() {
    let mut app = App::new(Vec::new(), "reviewer-pane");

    assert!(app.on_key(key(KeyCode::Enter)).is_none());
    assert!(app.on_key(key(KeyCode::Char('a'))).is_none());
    assert!(app.proposed().is_none());

    assert!(screen(&app).contains("No message selected."));
}

#[test]
fn the_body_pane_says_the_message_is_not_approved() {
    // The screen has to distinguish "you are reading this to decide" from
    // "this was approved", or the act of reading starts to feel like consent.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));

    assert!(screen(&app).contains("not yet approved"));
}

fn release(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Release,
        state: KeyEventState::NONE,
    }
}

#[test]
fn a_key_release_is_not_a_second_press() {
    // Windows reports a press and a release for the same physical key; Unix
    // terminals report only the press. Counting both would halve every
    // interaction on this screen, and the one that matters is the two-press
    // approval: the press would propose the delivery and the release would
    // confirm it, so a single keystroke would disclose a quarantined body to
    // an agent. The whole screen exists to make that impossible.
    let mut app = app();

    app.on_key(key(KeyCode::Enter));
    app.on_key(release(KeyCode::Enter));
    assert!(
        app.is_revealed(),
        "the release should not have hidden the body again"
    );

    assert!(app.on_key(key(KeyCode::Char('a'))).is_none());
    assert_eq!(app.focus(), Focus::Confirm);

    assert!(
        app.on_key(release(KeyCode::Char('a'))).is_none(),
        "releasing the proposing key must not confirm the decision"
    );
    assert_eq!(
        app.focus(),
        Focus::Confirm,
        "the screen should still be waiting for a deliberate confirmation"
    );
}

#[test]
fn a_release_cannot_confirm_a_proposed_decision() {
    // The same hazard reached from the other side: once a decision is
    // proposed, any key that is not `y` cancels it. A release arriving as a
    // key press would therefore cancel the proposal rather than confirm it —
    // harmless here, but it would make the screen unusable on Windows, where
    // every proposal would be cancelled by letting go of the key.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('d')));
    assert_eq!(app.focus(), Focus::Confirm);

    app.on_key(release(KeyCode::Char('d')));

    let outcome = app.on_key(key(KeyCode::Char('y')));
    assert_eq!(
        outcome,
        Some(Outcome::Decline {
            message_id: "msg-1".into()
        }),
        "the proposal should have survived the release"
    );
}
