use super::*;

#[test]
fn every_entry_invokes_the_one_hrc_binary() {
    // PRD requirement HRC-TECH-002: one self-contained executable serves the
    // CLI, the daemon, and every Herdr entry point. A manifest entry that
    // named a second program would break that on installation day.
    let manifest = manifest();

    assert_eq!(manifest.executable, "hrc");

    for entry in manifest.entries() {
        assert_eq!(
            entry.command.first().map(String::as_str),
            Some("herdr"),
            "`{}` must be an `hrc herdr ...` entry point",
            entry.id
        );
    }
}

#[test]
fn the_manifest_declares_the_four_entry_points_the_prd_lists() {
    // PRD section 13.2: startup, action, event, and pane.
    let manifest = manifest();

    assert_eq!(manifest.startup.command, ["herdr", "startup"]);
    assert_eq!(manifest.events.command, ["herdr", "event"]);
    assert_eq!(manifest.actions[0].command, ["herdr", "action", "inbox"]);
    assert_eq!(manifest.panes[0].command, ["herdr", "pane", "inbox"]);
}

#[test]
fn every_declared_pane_is_a_pane_this_plugin_can_draw() {
    let manifest = manifest();

    assert_eq!(manifest.panes.len(), crate::pane::Pane::ALL.len());

    for entry in &manifest.panes {
        let name = entry.command.last().expect("a pane entry names its pane");
        assert!(
            crate::pane::Pane::parse(name).is_some(),
            "the manifest advertises `{name}`, which the plugin cannot draw"
        );
    }
}

#[test]
fn entry_identifiers_are_unique() {
    let manifest = manifest();
    let mut ids: Vec<&str> = manifest
        .entries()
        .iter()
        .map(|entry| entry.id.as_str())
        .collect();

    let before = ids.len();
    ids.sort_unstable();
    ids.dedup();

    assert_eq!(
        ids.len(),
        before,
        "two manifest entries share an identifier"
    );
}

#[test]
fn the_manifest_serializes_to_json_herdr_can_read() {
    let rendered = serde_json::to_value(manifest()).expect("the manifest serializes");

    assert_eq!(rendered["id"], "herdr-remote-channel");
    assert_eq!(rendered["executable"], "hrc");
    assert!(rendered["panes"].is_array());
}
