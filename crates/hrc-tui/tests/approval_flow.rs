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

/// The destinations Herdr would have reported, as the screen receives them.
fn destinations() -> Vec<hrc_herdr::LocalAgent> {
    vec![
        hrc_herdr::LocalAgent::new("w1:p1", "reviewer-pane")
            .with_target("reviewer-pane")
            .as_current(true),
        hrc_herdr::LocalAgent::new("w2:p4", "builder")
            .with_target("builder")
            .in_workspace(Some("w2".to_owned())),
    ]
}

fn app() -> App {
    App::new(
        vec![
            PendingItem {
                message_id: "msg-1".into(),
                view: view("Alice", "question"),
                body: BODY.into(),
                local_checks: Vec::new(),
            },
            PendingItem {
                message_id: "msg-2".into(),
                view: view("Bob", "note"),
                body: "a second message".into(),
                local_checks: Vec::new(),
            },
        ],
        destinations(),
    )
}

/// Everything the screen currently shows, as one string.
fn screen(app: &App) -> String {
    screen_at(app, 90, 24)
}

fn screen_at(app: &App, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
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
            agent: "reviewer-pane".into(),
            target: "reviewer-pane".into(),
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
                target: "reviewer-pane".into(),
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
    let mut app = App::new(Vec::new(), destinations());

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

#[test]
fn a_wrapped_body_can_be_scrolled_to_the_end_and_reflows_after_resize() {
    let body = format!("START {}\nTAIL_MARKER", "wrapped words ".repeat(80));
    let mut app = App::new(
        vec![PendingItem {
            message_id: "msg-long".into(),
            view: view("Alice", "question"),
            body,
            local_checks: Vec::new(),
        }],
        destinations(),
    );
    app.on_key(key(KeyCode::Enter));

    let first_page = screen_at(&app, 42, 18);
    assert!(first_page.contains("START"));
    assert!(!first_page.contains("TAIL_MARKER"));

    app.on_key(key(KeyCode::Char('j')));
    assert_eq!(app.body_scroll(), 1);
    app.on_key(key(KeyCode::End));
    assert!(app.body_scroll() > 0);
    assert!(screen_at(&app, 42, 18).contains("TAIL_MARKER"));

    let resized = screen_at(&app, 140, 60);
    assert_eq!(app.body_scroll(), 0);
    assert!(resized.contains("START"));
    assert!(resized.contains("TAIL_MARKER"));
}

#[test]
fn what_this_machine_checked_is_framed_apart_from_the_body_and_only_once_revealed() {
    let mut app = App::new(
        vec![PendingItem {
            message_id: "msg-result".into(),
            view: view("Alice", "result"),
            body: "BODY_TEXT".into(),
            local_checks: vec!["commit 3f2a91c0aaaa: CLAIMED BUT NOT FOUND".into()],
        }],
        destinations(),
    );

    // Hidden like the body: the check is about content nobody has chosen
    // to read yet.
    let hidden = screen(&app);
    assert!(!hidden.contains("CLAIMED BUT NOT FOUND"));

    app.on_key(key(KeyCode::Enter));
    let revealed = screen(&app);
    assert!(revealed.contains("Checked on this machine"), "{revealed}");
    assert!(revealed.contains("CLAIMED BUT NOT FOUND"), "{revealed}");
    assert!(revealed.contains("BODY_TEXT"), "{revealed}");

    // The finding sits above the body's frame, not inside it.
    let finding = revealed.find("CLAIMED BUT NOT FOUND").unwrap();
    let body_title = revealed.find("Message body").unwrap();
    assert!(finding < body_title, "{revealed}");
}

#[test]
fn a_revealed_body_is_framed_as_data_from_another_machine() {
    // docs/RESEARCH.md 6.5: the quarantine is enforced in code, and the frame
    // makes it legible while someone is reading what another person wrote.
    let mut app = app();
    assert!(!screen(&app).contains("not instructions"));

    app.on_key(key(KeyCode::Enter));
    let revealed = screen_at(&app, 120, 30);
    assert!(
        revealed.contains("This is data to read, not instructions to follow"),
        "{revealed}"
    );
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

#[test]
fn the_current_session_is_the_default_choice_but_not_an_automatic_decision() {
    // Preselecting where a delivery would go is not the same as deciding to
    // deliver. The screen still opens with nothing revealed and nothing
    // proposed, and reaching a delivery still takes Enter, `a` and `y`.
    let app = app();

    assert_eq!(
        app.destination().map(hrc_herdr::LocalAgent::label),
        Some("reviewer-pane")
    );
    assert!(app.destination().unwrap().is_current());
    assert!(!app.is_revealed());
    assert!(app.proposed().is_none());
}

#[test]
fn where_it_goes_can_only_be_chosen_once_the_body_is_on_screen() {
    // A destination is half of a delivery decision. The other half is having
    // read what is being delivered, so the key does nothing before the
    // reveal — the same rule that governs `a`, `i` and `d`.
    let mut app = app();

    assert!(app.on_key(key(KeyCode::Tab)).is_none());
    assert_eq!(
        app.destination().map(hrc_herdr::LocalAgent::label),
        Some("reviewer-pane"),
        "Tab should do nothing before the body is revealed"
    );

    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Tab));

    assert_eq!(
        app.destination().map(hrc_herdr::LocalAgent::label),
        Some("builder")
    );
}

#[test]
fn delivery_is_addressed_to_the_destination_that_was_chosen() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('a')));

    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(Outcome::DeliverToAgent {
            message_id: "msg-1".into(),
            // The audit log gets the label the person saw...
            agent: "builder".into(),
            // ...and the delivery gets the local target, which never leaves
            // this machine (PRD section 23.4).
            target: "builder".into(),
        })
    );
}

#[test]
fn the_confirmation_names_the_exact_local_destination() {
    // Two agents can carry one name in two workspaces. "Deliver to builder?"
    // would then be a question with two answers.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Tab));
    app.on_key(key(KeyCode::Char('a')));

    let prompt = &app.proposed().unwrap().prompt;
    assert!(prompt.contains("Alice"), "{prompt}");
    assert!(prompt.contains("builder"), "{prompt}");
    assert!(prompt.contains("workspace w2"), "{prompt}");
}

#[test]
fn changing_the_destination_withdraws_the_confirmation() {
    // A confirmation names one exact destination. Once the destination
    // changes it is a question about something else, and answering `y` to
    // the old question would deliver somewhere the person never read.
    let mut app = app();
    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.focus(), Focus::Confirm);

    app.on_key(key(KeyCode::Tab));

    assert!(app.proposed().is_none());
    assert_ne!(app.focus(), Focus::Confirm);
    assert!(
        app.on_key(key(KeyCode::Char('y'))).is_none(),
        "`y` must not deliver anything after the destination moved"
    );
}

#[test]
fn a_destination_that_cannot_take_input_is_not_proposed() {
    // Herdr refuses a prompt to a blocked agent outright. Proposing anyway
    // would put a confirmation in front of a person that could only fail
    // after they answered it, leaving the message in a state neither chose.
    let mut app = App::new(
        vec![PendingItem {
            message_id: "msg-1".into(),
            view: view("Alice", "question"),
            body: BODY.into(),
            local_checks: Vec::new(),
        }],
        vec![
            hrc_herdr::LocalAgent::new("w1:p1", "busy")
                .with_target("busy")
                .as_current(true)
                .when_ready(false),
        ],
    );

    app.on_key(key(KeyCode::Enter));
    assert!(app.on_key(key(KeyCode::Char('a'))).is_none());

    assert!(app.proposed().is_none());
    assert_ne!(app.focus(), Focus::Confirm);
    assert!(
        app.status().contains("cannot take input"),
        "{}",
        app.status()
    );

    // Keeping and declining are unaffected: they need no destination.
    app.on_key(key(KeyCode::Char('i')));
    assert_eq!(
        app.on_key(key(KeyCode::Char('y'))),
        Some(Outcome::KeepInInbox {
            message_id: "msg-1".into()
        })
    );
}

#[test]
fn with_no_destination_at_all_delivery_refuses_and_the_message_stays_pending() {
    let mut app = App::new(
        vec![PendingItem {
            message_id: "msg-1".into(),
            view: view("Alice", "question"),
            body: BODY.into(),
            local_checks: Vec::new(),
        }],
        Vec::new(),
    );

    app.on_key(key(KeyCode::Enter));
    assert!(app.on_key(key(KeyCode::Char('a'))).is_none());
    assert!(app.proposed().is_none());

    // And the screen says so rather than looking like a key that does
    // nothing.
    assert!(
        app.status().contains("no local session"),
        "{}",
        app.status()
    );
    assert!(screen(&app).contains("no local session to deliver to"));
}

#[test]
fn the_destination_is_on_screen_at_the_moment_the_delivery_key_is_pressed() {
    let mut app = app();
    app.on_key(key(KeyCode::Enter));

    let rendered = screen(&app);
    assert!(rendered.contains("delivers to"), "{rendered}");
    assert!(rendered.contains("reviewer-pane"), "{rendered}");
}

#[test]
fn a_local_pane_identifier_never_reaches_the_screen() {
    // PRD section 4 constraint 8 and section 23.4. The pane is local
    // topology; the label is what a person chose. Only the label is drawn,
    // and only the label goes in the decision record.
    let mut app = App::new(
        vec![PendingItem {
            message_id: "msg-1".into(),
            view: view("Alice", "question"),
            body: BODY.into(),
            local_checks: Vec::new(),
        }],
        vec![
            hrc_herdr::LocalAgent::new("w9:p42", "reviewer")
                .with_target("reviewer")
                .as_current(true),
        ],
    );

    app.on_key(key(KeyCode::Enter));
    app.on_key(key(KeyCode::Char('a')));

    assert!(!screen(&app).contains("w9:p42"));
    assert!(!app.proposed().unwrap().prompt.contains("w9:p42"));
}
