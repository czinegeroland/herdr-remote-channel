use super::*;

#[test]
fn a_link_is_the_locator_and_a_marker() {
    assert_eq!(
        join_link("https://github.com/owner/name.git"),
        "https://github.com/owner/name.git#hrc-join"
    );
}

#[test]
fn a_link_round_trips_to_the_locator_it_names() {
    let locator = "https://github.com/owner/name.git";
    assert_eq!(locator_from(&join_link(locator)).as_deref(), Some(locator));
}

#[test]
fn a_bare_repository_url_is_not_ours() {
    // Claiming modified clicks on every GitHub link in every pane would be
    // stealing a gesture this plugin does not own.
    assert_eq!(locator_from("https://github.com/owner/name"), None);
    assert_eq!(locator_from("https://github.com/owner/name.git"), None);
}

#[test]
fn only_http_urls_are_read() {
    for clicked in [
        "file:///etc/passwd#hrc-join",
        "javascript:alert(1)#hrc-join",
        "hrc-join",
        "#hrc-join",
        "https://#hrc-join",
        "https:///owner/name#hrc-join",
    ] {
        assert_eq!(locator_from(clicked), None, "{clicked} was accepted");
    }
}

#[test]
fn a_clicked_url_is_bounded_and_free_of_control_characters() {
    // It arrives from a pane, which shows whatever a program wrote to it --
    // including something another person sent.
    let long = format!("https://example.com/{}#hrc-join", "a".repeat(MAX_URL));
    assert_eq!(locator_from(&long), None);

    assert_eq!(
        locator_from("https://example.com/\u{1b}[31mowner#hrc-join"),
        None
    );
    assert_eq!(locator_from("https://example.com/a\nb#hrc-join"), None);
}

#[test]
fn the_pattern_matches_what_the_builder_produces_and_not_much_else() {
    // Herdr compiles this, not us, so the test holds the shape rather than
    // the engine: anchored at both ends and requiring the marker.
    assert!(JOIN_PATTERN.starts_with('^'));
    assert!(JOIN_PATTERN.ends_with("#hrc-join$"));
    assert!(JOIN_PATTERN.contains("https?"));
}

#[test]
fn nothing_here_carries_an_invite_code() {
    // A URL is the worst place a secret can be: scrollback, shell history, a
    // clipboard manager, and whatever it was pasted into on the way over.
    let link = join_link("https://github.com/owner/name.git");
    assert!(!link.contains('?'), "a link takes no parameters: {link}");

    // And the reader produces exactly one thing, which is public.
    assert_eq!(
        locator_from("https://github.com/owner/name.git?code=secret#hrc-join").as_deref(),
        Some("https://github.com/owner/name.git?code=secret"),
        "a query is part of the locator, not a field this build reads"
    );
}
