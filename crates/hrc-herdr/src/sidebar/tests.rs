use super::*;

fn channel(local_name: &str, pending: usize, halted: bool) -> ChannelStatus {
    ChannelStatus {
        local_name: local_name.to_owned(),
        roster_epoch: 1,
        pending,
        halted,
    }
}

#[test]
fn the_prd_example_line_is_what_the_sidebar_produces() {
    // PRD section 23.1 prints this exact line as the suggested status.
    let sidebar = Sidebar {
        unread: 3,
        approvals: 1,
        synced_seconds_ago: Some(12),
        halted: 0,
        unanswered: 0,
        awaiting_answer: 0,
    };

    assert_eq!(
        sidebar.render(),
        "HRC: 3 unread | 1 approval | synced 12s ago"
    );
}

#[test]
fn approvals_are_summed_across_channels() {
    let sidebar = Sidebar::from_status(
        &[channel("team", 2, false), channel("ops", 3, false)],
        0,
        Some(0),
        0,
        0,
    );

    assert_eq!(sidebar.approvals, 5);
    assert_eq!(sidebar.halted, 0);
}

#[test]
fn a_halted_channel_is_the_first_thing_on_the_line() {
    // Section 26 requires the tamper halt to be visible and sticky. A person
    // scanning the sidebar must not have to read past an unread count.
    let sidebar = Sidebar::from_status(&[channel("team", 0, true)], 9, Some(4), 0, 0);

    assert!(sidebar.render().starts_with("HRC: 1 HALTED |"));
}

#[test]
fn a_channel_that_never_synced_says_so_rather_than_claiming_zero_seconds() {
    let sidebar = Sidebar::from_status(&[channel("team", 0, false)], 0, None, 0, 0);

    assert!(sidebar.render().ends_with("never synced"));
}

#[test]
fn elapsed_time_is_coarse_but_never_misleading() {
    assert_eq!(hrc_core::time::short_duration(0), "0s");
    assert_eq!(hrc_core::time::short_duration(59), "59s");
    assert_eq!(hrc_core::time::short_duration(60), "1m");
    assert_eq!(hrc_core::time::short_duration(3599), "59m");
    assert_eq!(hrc_core::time::short_duration(3600), "1h");
    assert_eq!(hrc_core::time::short_duration(86_399), "23h");
    assert_eq!(hrc_core::time::short_duration(86_400), "1d");
}

#[test]
fn only_one_of_something_is_singular() {
    assert_eq!(plural(0, "approval"), "0 approvals");
    assert_eq!(plural(1, "approval"), "1 approval");
    assert_eq!(plural(2, "approval"), "2 approvals");
}

#[test]
fn unanswered_questions_appear_on_the_line_only_when_there_are_some() {
    let quiet = Sidebar {
        unread: 0,
        approvals: 0,
        synced_seconds_ago: Some(1),
        halted: 0,
        unanswered: 0,
        awaiting_answer: 4,
    };

    // What this installation is owed is carried but not rendered: the line
    // has to stay short, and what a person can act on is what they owe.
    assert!(!quiet.render().contains("unanswered"));

    let owing = Sidebar {
        unanswered: 2,
        ..quiet
    };
    assert!(owing.render().contains("2 unanswered"));
}

#[test]
fn a_halt_still_comes_before_an_unanswered_count() {
    let sidebar = Sidebar {
        unread: 1,
        approvals: 1,
        synced_seconds_ago: Some(1),
        halted: 1,
        unanswered: 3,
        awaiting_answer: 0,
    };

    let line = sidebar.render();
    assert!(line.find("HALTED").unwrap() < line.find("unanswered").unwrap());
}
