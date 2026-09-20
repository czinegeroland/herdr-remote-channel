use super::*;

#[test]
fn an_empty_document_is_the_defaults() {
    let (config, problems) = parse("{}");
    assert_eq!(config, Config::default());
    assert!(problems.is_empty());
}

#[test]
fn a_file_may_set_one_thing_without_restating_the_rest() {
    let (config, problems) = parse(r#"{"inbox": {"open_at_startup": false}}"#);

    assert!(!config.open_inbox_at_startup);
    assert_eq!(config.inbox_share, Config::default().inbox_share);
    assert!(config.notifications);
    assert!(problems.is_empty());
}

#[test]
fn malformed_json_falls_back_to_the_defaults_and_says_so() {
    let (config, problems) = parse("{not json");

    assert_eq!(config, Config::default());
    assert_eq!(problems, vec![ConfigProblem::Malformed]);
}

#[test]
fn a_share_outside_the_range_is_refused_by_name() {
    for share in ["0.02", "0.99"] {
        let (config, problems) = parse(&format!(r#"{{"inbox": {{"share": {share}}}}}"#));

        assert_eq!(config.inbox_share, Config::default().inbox_share);
        assert_eq!(
            problems,
            vec![ConfigProblem::OutOfRange {
                setting: "inbox.share"
            }]
        );
    }
}

#[test]
fn the_bounds_themselves_are_honoured() {
    let (config, problems) = parse(&format!(r#"{{"inbox": {{"share": {MIN_SHARE}}}}}"#));
    assert_eq!(config.inbox_share, MIN_SHARE);
    assert!(problems.is_empty());

    let (config, problems) = parse(&format!(r#"{{"inbox": {{"share": {MAX_SHARE}}}}}"#));
    assert_eq!(config.inbox_share, MAX_SHARE);
    assert!(problems.is_empty());
}

#[test]
fn an_unknown_key_is_reported_and_its_neighbours_still_apply() {
    // A typo that silently does nothing is the thing that wastes an
    // afternoon, and a typo that discards the settings beside it is worse.
    let (config, problems) = parse(
        r#"{"inbox": {"open_at_startup": false, "shair": 0.5}, "colours": {"theme": "dark"}}"#,
    );

    assert!(!config.open_inbox_at_startup);
    assert!(problems.contains(&ConfigProblem::Unknown {
        key: "inbox.shair".into()
    }));
    assert!(problems.contains(&ConfigProblem::Unknown {
        key: "colours".into()
    }));
}

#[test]
fn a_value_of_the_wrong_type_is_malformed_rather_than_ignored() {
    let (config, problems) = parse(r#"{"inbox": {"share": "a quarter"}}"#);

    assert_eq!(config, Config::default());
    assert!(problems.contains(&ConfigProblem::Malformed));
}

#[test]
fn notifications_can_be_turned_off() {
    let (config, problems) = parse(r#"{"notifications": {"enabled": false}}"#);

    assert!(!config.notifications);
    assert!(problems.is_empty());
}

#[test]
fn every_problem_has_wording_naming_what_was_ignored() {
    assert!(
        ConfigProblem::OutOfRange {
            setting: "inbox.share"
        }
        .as_str()
        .contains("inbox.share")
    );
    assert!(
        ConfigProblem::Unknown {
            key: "colours".into()
        }
        .as_str()
        .contains("colours")
    );
}

#[test]
fn the_window_title_is_off_until_somebody_asks_for_it() {
    // It belongs to the client, not to this plugin. A default that took over
    // a surface we do not own would be the kind of behaviour that gets a
    // plugin uninstalled.
    assert!(!Config::default().window_title);
    assert!(Config::default().pane_token);

    let (config, problems) = parse(r#"{"indicator": {"window_title": true}}"#);
    assert!(config.window_title);
    assert!(config.pane_token, "the other surface is unaffected");
    assert!(problems.is_empty());
}

#[test]
fn the_pane_token_can_be_turned_off_on_its_own() {
    let (config, problems) = parse(r#"{"indicator": {"pane_token": false}}"#);

    assert!(!config.pane_token);
    assert!(!config.window_title);
    assert!(problems.is_empty());
}
