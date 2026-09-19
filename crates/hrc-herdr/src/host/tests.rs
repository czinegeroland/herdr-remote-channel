use super::*;

/// One `AgentInfo` as Herdr's schema shapes it.
fn info(name: Option<&str>, pane: &str, ready: bool, status: &str) -> Value {
    json!({
        "pane_id": pane,
        "tab_id": "w1:t1",
        "workspace_id": "w1",
        "terminal_id": "term-1",
        "name": name,
        "agent": "claude",
        "display_agent": "Claude",
        "agent_status": status,
        "interactive_ready": ready,
        "focused": pane == "w1:p1",
        "launch_pending": false,
        "revision": 1,
        "screen_detection_skipped": false,
        "state_labels": {},
        "tokens": {},
    })
}

fn listing(agents: Vec<Value>) -> String {
    json!({ "id": "req-1", "result": { "type": "agent_list", "agents": agents } }).to_string()
}

#[test]
fn a_request_carries_the_id_its_answer_is_checked_against() {
    let line = agent_list("req-1");

    assert!(line.ends_with('\n'), "requests are newline delimited");

    let parsed: Value = serde_json::from_str(line.trim_end()).expect("a request is JSON");
    assert_eq!(parsed["id"], "req-1");
    assert_eq!(parsed["method"], "agent.list");
}

#[test]
fn an_approved_body_travels_in_the_request_rather_than_on_a_command_line() {
    // The reason this module exists. `herdr agent prompt <target> <text>`
    // would put a decrypted message in argv, where anything that can run
    // `ps` could read it for as long as the process lived.
    let body = "the decrypted message";
    let line = agent_prompt("req-2", "reviewer", body);

    let parsed: Value = serde_json::from_str(line.trim_end()).expect("a request is JSON");
    assert_eq!(parsed["method"], "agent.prompt");
    assert_eq!(parsed["params"]["target"], "reviewer");
    assert_eq!(parsed["params"]["text"], body);
}

#[test]
fn a_response_for_another_request_is_not_accepted() {
    // One connection carries replies and subscription events, so a response
    // is matched rather than assumed. Acting on the wrong one is how a
    // delivery gets recorded against a message it never reached.
    let answer = json!({ "id": "req-9", "result": { "type": "agent_prompted" } }).to_string();

    assert_eq!(
        accepted(&answer, "req-2", "agent_prompted"),
        Err(HostError::Mismatched)
    );
}

#[test]
fn a_refusal_is_reported_with_the_code_herdr_gave() {
    // `agent_blocked` is the one that matters: Herdr refuses a prompt to a
    // blocked agent without sending any input, so a caller that treated this
    // as success would record a delivery that did not happen.
    let answer = json!({ "id": "req-2", "error": { "code": "agent_blocked" } }).to_string();

    assert_eq!(
        accepted(&answer, "req-2", "agent_prompted"),
        Err(HostError::Refused {
            code: "agent_blocked".to_owned()
        })
    );
}

#[test]
fn anything_unreadable_or_unexpected_fails_rather_than_passing() {
    assert_eq!(
        accepted("not json", "req-2", "agent_prompted"),
        Err(HostError::Unreadable)
    );
    assert_eq!(
        accepted(
            &json!({ "id": "req-2", "result": { "type": "pong" } }).to_string(),
            "req-2",
            "agent_prompted"
        ),
        Err(HostError::Unexpected)
    );
    assert_eq!(
        accepted(
            &json!({ "id": "req-2" }).to_string(),
            "req-2",
            "agent_prompted"
        ),
        Err(HostError::Unexpected)
    );
    assert_eq!(
        agents("{ not json", "req-1", None).unwrap_err(),
        HostError::Unreadable
    );

    // Valid JSON of the wrong shape carries no `id`, so it cannot be the
    // answer to anything. It fails as a mismatch rather than being read for
    // whatever it happens to contain.
    assert_eq!(
        agents("[]", "req-1", None).unwrap_err(),
        HostError::Mismatched
    );
}

#[test]
fn the_calling_pane_is_the_current_destination() {
    let line = listing(vec![
        info(Some("builder"), "w1:p2", true, "idle"),
        info(Some("reviewer"), "w1:p1", true, "idle"),
    ]);

    let destinations = agents(&line, "req-1", Some("w1:p2")).expect("a listing parses");

    // Sorted so the session the person is working in comes first. That is an
    // ordering, not a decision: the screen still opens with nothing proposed.
    assert_eq!(destinations[0].label(), "builder");
    assert!(destinations[0].is_current());
    assert!(!destinations[1].is_current());
}

#[test]
fn without_a_calling_pane_the_focused_agent_takes_that_place() {
    // A Herdr popup does not receive `HERDR_PANE_ID`, and the review screen
    // is a popup.
    let line = listing(vec![
        info(Some("builder"), "w1:p2", true, "idle"),
        info(Some("reviewer"), "w1:p1", true, "idle"),
    ]);

    let destinations = agents(&line, "req-1", None).expect("a listing parses");

    assert_eq!(destinations[0].label(), "reviewer");
    assert!(destinations[0].is_current());
}

#[test]
fn an_unnamed_agent_is_addressed_by_its_pane() {
    let line = listing(vec![info(None, "w1:p3", true, "idle")]);

    let destinations = agents(&line, "req-1", None).expect("a listing parses");

    assert_eq!(destinations[0].local_target(), "w1:p3");
    assert_eq!(destinations[0].label(), "Claude");
    assert_eq!(destinations[0].local_pane_id(), "w1:p3");
}

#[test]
fn a_named_agent_is_addressed_by_its_name() {
    // A name follows the agent across panes; a pane is whatever occupies it
    // now. Addressing the pane would deliver to a replacement.
    let line = listing(vec![info(Some("reviewer"), "w1:p1", true, "idle")]);

    let destinations = agents(&line, "req-1", None).expect("a listing parses");

    assert_eq!(destinations[0].local_target(), "reviewer");
}

#[test]
fn a_blocked_or_unready_agent_is_listed_but_not_ready() {
    let line = listing(vec![
        info(Some("busy"), "w1:p4", true, "blocked"),
        info(Some("starting"), "w1:p5", false, "idle"),
        info(Some("free"), "w1:p6", true, "idle"),
    ]);

    let destinations = agents(&line, "req-1", None).expect("a listing parses");

    assert_eq!(destinations.len(), 3, "nothing is hidden");
    assert!(!destinations[0].is_ready());
    assert!(!destinations[1].is_ready());
    assert!(destinations[2].is_ready());
}

#[test]
fn an_entry_with_no_pane_is_dropped() {
    // A destination that cannot be addressed is not a destination, and
    // listing one would let a person confirm a delivery with nowhere to go.
    let line = listing(vec![
        json!({ "name": "ghost" }),
        info(Some("real"), "w1:p1", true, "idle"),
    ]);

    let destinations = agents(&line, "req-1", None).expect("a listing parses");

    assert_eq!(destinations.len(), 1);
    assert_eq!(destinations[0].label(), "real");
}

#[test]
fn a_terminal_title_never_becomes_a_destination_label() {
    // A title is whatever the program in the pane last wrote, which can be
    // output from a message this very screen is deciding about.
    let mut entry = info(None, "w1:p7", true, "idle");
    entry["display_agent"] = Value::Null;
    entry["agent"] = Value::Null;
    entry["title"] = json!("SENDER CHOSE THIS");
    entry["terminal_title"] = json!("SENDER CHOSE THIS");

    let destinations = agents(&listing(vec![entry]), "req-1", None).expect("a listing parses");

    assert_eq!(destinations[0].label(), "w1:p7");
    assert!(!destinations[0].label().contains("SENDER"));
}

#[test]
fn an_empty_installation_offers_no_destination() {
    let destinations = agents(&listing(Vec::new()), "req-1", None).expect("a listing parses");

    assert!(destinations.is_empty());
}

#[test]
fn opening_the_inbox_asks_for_a_split_without_focus() {
    // On startup a person may already be typing. A side view that grabs the
    // keyboard to announce itself is the behaviour that gets a plugin
    // uninstalled.
    let line = open_inbox("req-3");
    let parsed: Value = serde_json::from_str(line.trim_end()).expect("a request is JSON");

    assert_eq!(parsed["method"], "plugin.pane.open");
    assert_eq!(parsed["params"]["entrypoint"], "inbox");
    assert_eq!(parsed["params"]["placement"], "split");
    assert_eq!(parsed["params"]["focus"], false);
    assert_eq!(parsed["params"]["plugin_id"], crate::manifest::PLUGIN_ID);
}

#[test]
fn the_pane_an_open_created_is_read_back() {
    // Recorded so the live handoff that re-runs startup hooks while keeping
    // panes alive does not leave two inboxes side by side.
    let answer = json!({
        "id": "req-3",
        "result": {
            "type": "plugin_pane_opened",
            "plugin_pane": {
                "plugin_id": "herdr-remote-channel",
                "entrypoint": "inbox",
                "pane": { "pane_id": "wG:p2", "tab_id": "wG:t1", "workspace_id": "wG" },
            },
        },
    })
    .to_string();

    assert_eq!(opened_pane(&answer, "req-3").unwrap(), "wG:p2");
}

#[test]
fn an_open_that_did_not_say_which_pane_is_not_guessed_at() {
    // A remembered pane that is not the one Herdr opened would suppress the
    // next startup's open while nothing is on screen.
    for answer in [
        json!({ "id": "req-3", "result": { "type": "plugin_pane_opened" } }),
        json!({ "id": "req-3", "result": { "type": "plugin_pane_opened",
            "plugin_pane": { "pane": { "pane_id": "" } } } }),
        json!({ "id": "req-3", "result": { "type": "pong" } }),
    ] {
        assert_eq!(
            opened_pane(&answer.to_string(), "req-3"),
            Err(HostError::Unexpected)
        );
    }
}
