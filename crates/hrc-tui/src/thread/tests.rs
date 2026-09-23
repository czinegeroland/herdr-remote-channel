use super::*;

use crossterm::event::{KeyEventState, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent {
        code,
        modifiers: KeyModifiers::NONE,
        kind: KeyEventKind::Press,
        state: KeyEventState::NONE,
    }
}

fn screen(app: &ThreadApp, width: u16, height: u16) -> String {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::view::render_thread(frame, app))
        .unwrap();
    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

fn entries(count: usize) -> Vec<ThreadEntry> {
    (0..count)
        .map(|index| ThreadEntry {
            heading: format!("alice · note {index}"),
            body: format!("BODY_{index}"),
        })
        .collect()
}

#[test]
fn a_thread_reads_top_to_bottom_with_headings_above_bodies() {
    let app = ThreadApp::new("Thread in project", entries(2));
    let shown = screen(&app, 60, 20);

    assert!(shown.contains("Thread in project"), "{shown}");
    let first = shown.find("alice · note 0").unwrap();
    let body = shown.find("BODY_0").unwrap();
    let second = shown.find("alice · note 1").unwrap();
    assert!(first < body && body < second, "{shown}");
}

#[test]
fn the_only_outcome_is_leaving() {
    // Read-only by construction: nothing a key does here decides anything.
    let mut app = ThreadApp::new("t", entries(1));
    for code in [
        KeyCode::Enter,
        KeyCode::Char('a'),
        KeyCode::Char('d'),
        KeyCode::Char('i'),
        KeyCode::Tab,
    ] {
        assert_eq!(app.on_key(key(code)), None, "{code:?}");
    }
    assert_eq!(app.on_key(key(KeyCode::Char('q'))), Some(()));
    assert_eq!(app.on_key(key(KeyCode::Esc)), Some(()));
}

#[test]
fn a_long_thread_scrolls_to_its_end_and_back() {
    let app_entries = entries(30);
    let mut app = ThreadApp::new("t", app_entries);
    let first = screen(&app, 60, 12);
    assert!(first.contains("BODY_0") && !first.contains("BODY_29"));

    app.on_key(key(KeyCode::End));
    let last = screen(&app, 60, 12);
    assert!(last.contains("BODY_29"), "{last}");

    app.on_key(key(KeyCode::Home));
    assert_eq!(app.scroll(), 0);
}

#[test]
fn an_empty_thread_says_so() {
    let app = ThreadApp::new("t", Vec::new());
    assert!(screen(&app, 60, 10).contains("Nothing in this thread yet"));
}
