use super::*;

#[test]
fn focusing_a_workspace_refreshes_the_sidebar() {
    let event = HostEvent::parse("workspace.focused").expect("the subscribed event parses");

    assert_eq!(event, HostEvent::WorkspaceFocused);
    assert_eq!(event.reaction(), Reaction::RefreshStatus);
}

#[test]
fn surrounding_whitespace_does_not_hide_an_event() {
    // The name arrives through an environment variable, which is one of the
    // easier places for a trailing newline to survive.
    assert_eq!(
        HostEvent::parse(" workspace.focused\n"),
        Some(HostEvent::WorkspaceFocused)
    );
}

#[test]
fn an_event_this_plugin_did_not_subscribe_to_is_ignored() {
    for name in [
        "workspace.created",
        "workspace.closed",
        "worktree.created",
        "tab.focused",
        "approve_everything",
        "",
    ] {
        assert_eq!(HostEvent::parse(name), None, "`{name}` should not parse");
        assert_eq!(reaction_to(Some(name)), Reaction::Ignore);
    }
}

#[test]
fn a_hook_invoked_with_no_event_named_does_nothing() {
    assert_eq!(reaction_to(None), Reaction::Ignore);
}

#[test]
fn every_subscribed_event_uses_the_host_dotted_naming() {
    for event in HostEvent::ALL {
        let name = event.as_str();
        assert!(
            name.contains('.'),
            "`{name}` is not a Herdr event name; the host namespaces them"
        );
        assert_eq!(name, name.trim());
    }
}

#[test]
fn no_reaction_can_approve_deliver_or_reveal_anything() {
    // The whole point of the type. If a variant is ever added that could,
    // this assertion is where the reviewer is meant to stop and think.
    for reaction in [Reaction::RefreshStatus, Reaction::Ignore] {
        let rendered = serde_json::to_string(&reaction).expect("a reaction serializes");

        for forbidden in ["approve", "deliver", "body", "authorization"] {
            assert!(
                !rendered.contains(forbidden),
                "`{rendered}` must not mention `{forbidden}`"
            );
        }
    }
}
