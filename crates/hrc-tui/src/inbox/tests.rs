use super::*;
use crossterm::event::KeyEventState;
use hrc_herdr::inbox::{LocalDecision, SecretScan, Verification};

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

/// Twenty-six Crockford base32 characters, so the identifier is a real ULID
/// and the inbox is exercised on the shape it will actually see.
fn ulid(suffix: char) -> String {
    format!("01ARZ3NDEKTSV4RRFFQ69G5F{suffix}V")
}

fn row(
    message_id: &str,
    sender: &str,
    arrival_at: &str,
    disposition: InboxDisposition,
) -> InboxRow {
    InboxRow {
        message_id: message_id.to_owned(),
        message_label: hrc_core::gate::message_label(message_id),
        sender_principal: format!("principal-{sender}"),
        sender_local_name: sender.to_owned(),
        kind: "task".to_owned(),
        thread_label: "unknown thread".to_owned(),
        channel_local_name: "project".to_owned(),
        arrival_at: arrival_at.to_owned(),
        expires_at: None,
        endpoint_label: String::new(),
        prompt_request: false,
        attachment_count: 0,
        attachment_bytes: 0,
        verification: Verification::Verified,
        secret_scan: SecretScan::NotApplicable,
        disposition,
        decisions: match disposition {
            InboxDisposition::Pending => vec![LocalDecision::DeliverToAgent],
            _ => Vec::new(),
        },
    }
}

fn rows() -> Vec<InboxRow> {
    vec![
        row(
            &ulid('A'),
            "alice",
            "2026-01-01T10:00:00Z",
            InboxDisposition::Pending,
        ),
        row(
            &ulid('B'),
            "bob",
            "2026-01-01T10:05:00Z",
            InboxDisposition::Pending,
        ),
        row(
            &ulid('C'),
            "carol",
            "2026-01-01T10:10:00Z",
            InboxDisposition::Delivered,
        ),
    ]
}

fn app() -> InboxApp {
    InboxApp::new("project", rows())
}

fn screen(app: &InboxApp, width: u16, height: u16) -> String {
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| crate::view::render_inbox(frame, app, "2026-01-01T10:12:00Z"))
        .unwrap();

    terminal
        .backend()
        .buffer()
        .content()
        .iter()
        .map(|cell| cell.symbol())
        .collect()
}

#[test]
fn the_first_pending_row_is_selected_and_nothing_is_decided_by_opening() {
    let app = app();

    assert_eq!(app.selected_message_id(), Some(ulid('A').as_str()));
    assert_eq!(app.filter(), InboxFilter::Pending);
    assert_eq!(app.pending(), 2);
}

#[test]
fn moving_the_selection_works_with_arrows_and_with_j_and_k() {
    let mut app = app();

    assert!(app.on_key(key(KeyCode::Down)).is_none());
    assert_eq!(app.selected_message_id(), Some(ulid('B').as_str()));

    assert!(app.on_key(key(KeyCode::Char('k'))).is_none());
    assert_eq!(app.selected_message_id(), Some(ulid('A').as_str()));

    assert!(app.on_key(key(KeyCode::Char('j'))).is_none());
    assert_eq!(app.selected_message_id(), Some(ulid('B').as_str()));

    // The last visible row is the end of the list, not a wrap to the top: a
    // person holding `j` should come to rest, not cycle past what they were
    // looking for.
    assert!(app.on_key(key(KeyCode::Char('j'))).is_none());
    assert_eq!(app.selected_message_id(), Some(ulid('B').as_str()));
}

#[test]
fn filters_change_which_rows_are_visible() {
    let mut app = app();
    assert_eq!(app.visible().len(), 2);

    app.on_key(key(KeyCode::Char('a')));
    assert_eq!(app.filter(), InboxFilter::All);
    assert_eq!(app.visible().len(), 3);

    app.on_key(key(KeyCode::Char('u')));
    assert_eq!(app.filter(), InboxFilter::Unread);
    assert_eq!(app.visible().len(), 2);

    app.on_key(key(KeyCode::Char('p')));
    assert_eq!(app.filter(), InboxFilter::Pending);
    assert_eq!(app.visible().len(), 2);
}

#[test]
fn enter_asks_to_review_exactly_the_selected_message() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));

    assert_eq!(
        app.on_key(key(KeyCode::Enter)),
        Some(InboxOutcome::Review {
            message_id: ulid('B'),
        })
    );
}

#[test]
fn no_key_produces_anything_but_a_review_request_or_a_quit() {
    // The point of this test is not that these particular keys are inert. It
    // is that `InboxOutcome` has no variant that could approve, decline,
    // reveal, or deliver, so no key handler change can make one — the type
    // is the boundary, and this asserts the boundary rather than the keys.
    let mut app = app();

    for code in [
        KeyCode::Char('y'),
        KeyCode::Char('Y'),
        KeyCode::Char('d'),
        KeyCode::Char('x'),
        KeyCode::Char(' '),
        KeyCode::Tab,
        KeyCode::Backspace,
        KeyCode::Delete,
        KeyCode::Home,
    ] {
        match app.on_key(key(code)) {
            None => {}
            Some(InboxOutcome::Quit) | Some(InboxOutcome::Review { .. }) => {
                panic!("{code:?} should not reach an outcome")
            }
        }
    }
}

#[test]
fn escape_and_q_and_control_c_all_leave() {
    for code in [KeyCode::Esc, KeyCode::Char('q')] {
        let mut app = app();
        assert_eq!(app.on_key(key(code)), Some(InboxOutcome::Quit));
    }

    let mut app = app();
    let mut interrupt = key(KeyCode::Char('c'));
    interrupt.modifiers = KeyModifiers::CONTROL;
    assert_eq!(app.on_key(interrupt), Some(InboxOutcome::Quit));
}

#[test]
fn a_key_release_does_nothing() {
    // Windows delivers a release for every press. Acting on both would move
    // the selection twice per key, and once `Enter` opens a popup it would
    // open two of them.
    let mut app = app();

    assert!(app.on_key(release(KeyCode::Down)).is_none());
    assert_eq!(app.selected_message_id(), Some(ulid('A').as_str()));

    assert!(app.on_key(release(KeyCode::Enter)).is_none());
}

#[test]
fn a_new_arrival_does_not_move_the_selection() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected_message_id(), Some(ulid('B').as_str()));

    let mut next = rows();
    // Arriving earlier than everything on screen, so it sorts to the top and
    // every index shifts by one. Tracking by index would move the selection
    // onto a message the person never chose.
    next.insert(
        0,
        row(
            &ulid('D'),
            "dave",
            "2026-01-01T09:00:00Z",
            InboxDisposition::Pending,
        ),
    );
    app.refresh(next);

    assert_eq!(app.selected_message_id(), Some(ulid('B').as_str()));
    assert_eq!(app.visible().len(), 3);
}

#[test]
fn a_changed_disposition_replaces_the_row_rather_than_duplicating_it() {
    let mut app = app();
    app.on_key(key(KeyCode::Char('a')));

    let mut next = rows();
    next[0].disposition = InboxDisposition::Declined;
    next[0].decisions = Vec::new();
    app.refresh(next);

    assert_eq!(app.visible().len(), 3);
    assert_eq!(app.pending(), 1);
    assert_eq!(
        app.selected().map(|row| row.disposition),
        Some(InboxDisposition::Declined)
    );
}

#[test]
fn a_disappeared_row_hands_the_selection_to_its_neighbour() {
    let mut app = app();
    app.on_key(key(KeyCode::Down));
    assert_eq!(app.selected_message_id(), Some(ulid('B').as_str()));

    // Decided elsewhere while the pane was open, so it leaves the pending
    // filter. Position 1 is now past the end, so the last row takes it.
    let mut next = rows();
    next[1].disposition = InboxDisposition::Delivered;
    next[1].decisions = Vec::new();
    app.refresh(next);

    assert_eq!(app.selected_message_id(), Some(ulid('A').as_str()));
}

#[test]
fn an_empty_inbox_selects_nothing_and_enter_decides_nothing() {
    let mut app = InboxApp::new("project", Vec::new());

    assert_eq!(app.selected_message_id(), None);
    assert!(app.on_key(key(KeyCode::Enter)).is_none());
    assert!(app.on_key(key(KeyCode::Down)).is_none());
    assert_eq!(app.selected_message_id(), None);
}

#[test]
fn a_failed_refresh_keeps_the_last_rows_and_says_so() {
    // An inbox that could not be read and an inbox with nothing in it look
    // identical if the failure is swallowed, and they mean opposite things.
    let mut app = app();
    app.refresh_failed("database is locked");

    assert!(app.stale());
    assert_eq!(app.visible().len(), 2);
    assert!(app.status().contains("database is locked"));

    // The frame says the rows may be out of date, and the footer says why.
    let rendered = screen(&app, 46, 10);
    assert!(rendered.contains("COULD NOT REFRESH"), "{rendered}");
    assert!(rendered.contains("database is locked"), "{rendered}");

    app.refresh(rows());
    assert!(!app.stale());
}

#[test]
fn the_refresh_key_asks_the_runner_rather_than_reading_anything_itself() {
    let mut app = app();
    assert!(!app.refresh_requested());

    assert!(app.on_key(key(KeyCode::Char('R'))).is_none());
    assert!(app.refresh_requested());

    app.refresh(rows());
    assert!(!app.refresh_requested());
}

#[test]
fn one_row_and_many_rows_both_render() {
    let one = InboxApp::new(
        "project",
        vec![row(
            &ulid('A'),
            "alice",
            "2026-01-01T10:00:00Z",
            InboxDisposition::Pending,
        )],
    );
    let rendered = screen(&one, 46, 10);
    assert!(rendered.contains("alice"), "{rendered}");
    assert!(rendered.contains("task"), "{rendered}");
    // Not `PENDING` on every row: in the pending filter that is a column
    // repeating what the title already says.
    assert!(!rendered.contains("PENDING"), "{rendered}");

    let mut many = app();
    many.on_key(key(KeyCode::Char('a')));
    let rendered = screen(&many, 46, 10);
    for sender in ["alice", "bob", "carol"] {
        assert!(
            rendered.contains(sender),
            "{sender} is missing from {rendered}"
        );
    }
    // A decided row still says so, because that is what changed about it.
    assert!(rendered.contains("DELIVERED"), "{rendered}");
}

#[test]
fn an_empty_filter_says_which_emptiness_it_is() {
    let mut decided = rows();
    for row in &mut decided {
        row.disposition = InboxDisposition::Delivered;
        row.decisions = Vec::new();
    }

    // Each emptiness says which one it is, so a person who filtered to
    // pending and sees nothing knows whether that means nothing arrived or
    // nothing is waiting on them.
    let app = InboxApp::new("project", decided);
    assert!(screen(&app, 46, 10).contains("Nothing is waiting on you"));

    let mut empty = InboxApp::new("project", Vec::new());
    empty.on_key(key(KeyCode::Char('a')));
    assert!(screen(&empty, 46, 10).contains("Nothing has arrived yet"));

    let mut unread = InboxApp::new("project", Vec::new());
    unread.on_key(key(KeyCode::Char('u')));
    assert!(screen(&unread, 46, 10).contains("Nothing unread"));
}

#[test]
fn a_narrow_split_still_shows_the_selection_and_never_wraps_a_row() {
    let app = app();

    // Twelve columns is narrower than any real split, and the point is that
    // it degrades by losing the right-hand columns rather than by wrapping
    // one message across three lines and misaligning everything below it.
    let rendered = screen(&app, 12, 8);
    assert!(rendered.contains('>'));
    assert!(rendered.contains("ali"));

    // One column and one row is past the point of usefulness; it must still
    // not panic, because a person dragging a split makes it happen.
    let _ = screen(&app, 1, 1);
    let _ = screen(&app, 3, 2);
}

#[test]
fn the_rendered_buffer_holds_no_sender_chosen_text() {
    // The row type carries no body, subject, or attachment name to render,
    // and the only sender-supplied strings it does carry are put through a
    // validator first. This asserts the outcome of that: nothing a sender
    // wrote reaches the buffer.
    let mut rows = rows();
    rows[0].message_id = "<script>SENDER CHOSE THIS</script>".to_owned();
    rows[0].message_label = hrc_core::gate::message_label(&rows[0].message_id);
    rows[0].thread_label = hrc_core::gate::thread_label(Some("SENDER THREAD"));
    rows[0].endpoint_label = hrc_core::gate::endpoint_label(Some("SENDER ENDPOINT"));

    let mut app = InboxApp::new("project", rows);
    app.on_key(key(KeyCode::Char('a')));

    let rendered = screen(&app, 80, 12);
    for sender_text in ["SENDER CHOSE THIS", "SENDER THREAD", "SENDER ENDPOINT"] {
        assert!(
            !rendered.contains(sender_text),
            "{sender_text} reached the side view"
        );
    }
}

#[test]
fn a_malformed_identifier_still_selects_the_right_row() {
    // The stored identifier keys the row even when it is not a ULID,
    // because that is how the right message is targeted. What changes is
    // only what may be printed.
    let mut rows = rows();
    rows[0].message_id = "not-a-ulid".to_owned();
    rows[0].message_label = hrc_core::gate::message_label("not-a-ulid");

    let app = InboxApp::new("project", rows);
    assert_eq!(app.selected_message_id(), Some("not-a-ulid"));
    assert_eq!(
        app.selected().map(|row| row.message_label.as_str()),
        Some(hrc_core::gate::UNKNOWN_MESSAGE)
    );
}

#[test]
fn age_is_coarse_and_never_negative() {
    assert_eq!(age("2026-01-01T10:00:00Z", "2026-01-01T10:00:30Z"), "30s");
    assert_eq!(age("2026-01-01T10:00:00Z", "2026-01-01T10:02:00Z"), "2m");
    assert_eq!(age("2026-01-01T09:00:00Z", "2026-01-01T10:00:00Z"), "1h");
    assert_eq!(age("2025-12-30T10:00:00Z", "2026-01-01T10:00:00Z"), "2d");

    // A clock that moved backwards is not a message from the future.
    assert_eq!(age("2026-01-01T10:05:00Z", "2026-01-01T10:00:00Z"), "0s");

    // A timestamp this build cannot parse gets a fixed label rather than
    // being printed raw.
    assert_eq!(age("whenever", "2026-01-01T10:00:00Z"), UNKNOWN_AGE);
    assert_eq!(age("2026-01-01T10:00:00Z", "whenever"), UNKNOWN_AGE);
}

#[test]
fn a_long_channel_name_cannot_break_the_pane_border() {
    // The real one, from the screenshot that found this: a channel created
    // from `owner/name` is recorded under its expanded URL, and the title
    // ran past the border and broke the frame.
    let app = InboxApp::new(
        "https://github.com/czinegeroland/hrc-test.git",
        vec![row(
            &ulid('A'),
            "alice",
            "2026-01-01T10:00:00Z",
            InboxDisposition::Pending,
        )],
    );

    let rendered = screen(&app, 46, 10);

    // Shortened to something a person recognizes...
    assert!(rendered.contains("czinegeroland/hrc-test"), "{rendered}");
    // ...with no trace of the URL it came from.
    assert!(!rendered.contains("https://"), "{rendered}");
    assert!(!rendered.contains(".git"), "{rendered}");

    // And the frame is intact: the top row is the border, one title aside,
    // and no line is longer than the pane.
    for line in rendered_rows(&rendered, 46) {
        assert_eq!(line.chars().count(), 46, "a row outran the pane: {line:?}");
    }
}

#[test]
fn a_title_gives_up_the_name_before_the_filter() {
    // Which rows are on show is the part a person needs when the list looks
    // emptier than they expected, so it is the last thing to go.
    let app = InboxApp::new("a-very-long-channel-name-indeed", vec![]);

    for width in [40, 24, 16, 12, 8, 4, 2, 1] {
        let rendered = screen(&app, width, 6);
        for line in rendered_rows(&rendered, width) {
            assert_eq!(
                line.chars().count(),
                width as usize,
                "width {width} produced {line:?}"
            );
        }
    }

    // At a width that fits something, the filter survives and the name does
    // not take the space from it.
    assert!(screen(&app, 16, 6).contains("pending"));
}

#[test]
fn a_prompt_request_is_marked_because_it_blocks_a_person() {
    // Every other kind waits for someone. A prompt request is the one that
    // stops an agent until a person decides, so it is the one row that has to
    // be findable at a glance — marked in text, not colour (PRD section 27).
    let mut rows = rows();
    rows[1].prompt_request = true;

    let mut app = InboxApp::new("project", rows);
    app.on_key(key(KeyCode::Char('a')));

    let rendered = screen(&app, 46, 10);
    let marked: Vec<String> = rendered_rows(&rendered, 46)
        .into_iter()
        .filter(|line| line.contains('!'))
        .collect();

    assert_eq!(marked.len(), 1, "exactly one row is urgent: {rendered}");
    assert!(marked[0].contains("bob"), "{:?}", marked[0]);
}

#[test]
fn a_decided_prompt_request_is_no_longer_urgent() {
    // The mark means "this is waiting on you". Once it has been decided it is
    // not, however it arrived.
    let mut rows = rows();
    rows[2].prompt_request = true;
    assert_eq!(rows[2].disposition, InboxDisposition::Delivered);

    let mut app = InboxApp::new("project", rows);
    app.on_key(key(KeyCode::Char('a')));

    let rendered = screen(&app, 46, 10);
    for line in rendered_rows(&rendered, 46) {
        if line.contains("carol") {
            assert!(!line.contains('!'), "{line:?}");
        }
    }
}

#[test]
fn the_state_column_shows_what_changes_the_decision() {
    // Not the disposition on every line. What earns the rightmost column is
    // whatever a person would act on differently: that it can no longer be
    // acted on, that it carries files, or that it is already dealt with.
    let mut carrying = rows();
    carrying[0].attachment_count = 3;
    carrying[1].verification = Verification::Expired;

    let mut app = InboxApp::new("project", carrying);
    app.on_key(key(KeyCode::Char('a')));

    let rendered = screen(&app, 60, 10);
    for line in rendered_rows(&rendered, 60) {
        if line.contains("alice") {
            assert!(line.contains("3 files"), "{line:?}");
        }
        if line.contains("bob") {
            assert!(line.contains("EXPIRED"), "{line:?}");
        }
    }

    // One attachment is not "1 files".
    let mut single = rows();
    single[0].attachment_count = 1;
    let app = InboxApp::new("project", single);
    assert!(screen(&app, 60, 10).contains("1 file "));
}

#[test]
fn the_channel_health_line_rides_on_the_frame() {
    // The section 23.1 indicator, which was computed and handed to a host
    // surface that does not exist. The split is the surface it described.
    let mut app = InboxApp::new("project", rows());
    app.set_health(hrc_herdr::Sidebar {
        unread: 2,
        approvals: 2,
        synced_seconds_ago: Some(12),
        halted: 0,
        unanswered: 0,
        awaiting_answer: 0,
    });

    let rendered = screen(&app, 60, 10);
    assert!(rendered.contains("2 waiting on you"), "{rendered}");
    assert!(rendered.contains("2 unread"), "{rendered}");
    assert!(rendered.contains("synced 12s ago"), "{rendered}");
}

#[test]
fn a_halted_channel_takes_the_whole_health_line() {
    // Section 26 makes a tamper halt sticky and visible. A count of unread
    // notes is not what someone needs to read first when synchronization
    // stopped because the history was rewritten.
    let mut app = InboxApp::new("project", rows());
    app.set_health(hrc_herdr::Sidebar {
        unread: 9,
        approvals: 9,
        synced_seconds_ago: Some(4),
        halted: 1,
        unanswered: 0,
        awaiting_answer: 0,
    });

    let rendered = screen(&app, 60, 10);
    assert!(rendered.contains("HALTED"), "{rendered}");
    assert!(!rendered.contains("9 unread"), "{rendered}");
}

#[test]
fn the_footer_never_takes_a_second_line() {
    // The full key list is sixty-three characters. At any pane narrower than
    // a half window it wrapped and pushed the list up, spending a row of a
    // side view to say what the keys are.
    let mut app = InboxApp::new("project", rows());

    for width in [80, 60, 46, 32, 24, 16] {
        let rows_drawn = rendered_rows(&screen(&app, width, 8), width);
        let footer = rows_drawn.last().expect("a footer row").clone();
        assert_eq!(footer.chars().count(), width as usize, "{footer:?}");
    }

    // `?` swaps in the full list, and swaps it back out again.
    app.on_key(key(KeyCode::Char('?')));
    assert!(app.status().contains("R: refresh"), "{}", app.status());
    app.on_key(key(KeyCode::Char('?')));
    assert!(!app.status().contains("R: refresh"), "{}", app.status());
}

/// The rendered buffer split back into rows.
fn rendered_rows(rendered: &str, width: u16) -> Vec<String> {
    rendered
        .chars()
        .collect::<Vec<char>>()
        .chunks(width as usize)
        .map(|chunk| chunk.iter().collect())
        .collect()
}

#[test]
fn the_health_line_names_questions_nobody_answered() {
    let mut app = InboxApp::new("owner/channel".to_owned(), Vec::new());
    app.set_health(hrc_herdr::Sidebar {
        unread: 0,
        approvals: 0,
        synced_seconds_ago: Some(3),
        halted: 0,
        unanswered: 2,
        awaiting_answer: 1,
    });

    // A question already delivered and never answered leaves nothing in a
    // pending list, so this line is the only place it appears at all.
    assert!(
        app.health_line().contains("2 unanswered"),
        "{}",
        app.health_line()
    );
}

#[test]
fn a_halted_channel_still_takes_the_whole_health_line() {
    let mut app = InboxApp::new("owner/channel".to_owned(), Vec::new());
    app.set_health(hrc_herdr::Sidebar {
        unread: 5,
        approvals: 2,
        synced_seconds_ago: Some(3),
        halted: 1,
        unanswered: 9,
        awaiting_answer: 0,
    });

    assert_eq!(app.health_line(), "HALTED: published history was rewritten");
}
