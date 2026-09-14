use super::*;

#[test]
fn every_entry_invokes_the_one_hrc_binary() {
    // PRD requirement HRC-TECH-002: one self-contained executable serves the
    // CLI, the daemon, and every Herdr entry point. A manifest entry that
    // named a second program would break that on installation day.
    let manifest = manifest();

    for command in manifest.commands() {
        assert_eq!(
            command.first().map(String::as_str),
            Some(EXECUTABLE),
            "`{command:?}` must run the plugin's own `hrc`"
        );
        assert_eq!(
            command.get(1).map(String::as_str),
            Some("herdr"),
            "`{command:?}` must be an `hrc herdr ...` entry point"
        );
    }
}

#[test]
fn the_binary_is_resolved_from_the_plugin_root_rather_than_the_path() {
    // Herdr runs plugin commands with the plugin directory as the working
    // directory. Naming a bare `hrc` would make the plugin depend on the
    // install prefix being on whatever PATH the host inherited, which is a
    // silent failure on a machine where it is not.
    assert!(
        EXECUTABLE.contains('/'),
        "`{EXECUTABLE}` would be looked up on PATH"
    );
    assert!(
        !EXECUTABLE.starts_with('/'),
        "`{EXECUTABLE}` must be relative to the plugin root"
    );
}

#[test]
fn the_manifest_declares_the_four_entry_points_the_prd_lists() {
    // PRD section 13.2: startup, action, event, and pane.
    let manifest = manifest();

    let tail = |command: &[String]| command[1..].to_vec();

    assert_eq!(tail(&manifest.startup[0].command), ["herdr", "startup"]);
    assert_eq!(tail(&manifest.events[0].command), ["herdr", "event"]);
    assert_eq!(
        tail(&manifest.actions[0].command),
        ["herdr", "action", "inbox"]
    );
    assert_eq!(tail(&manifest.panes[0].command), ["herdr", "pane", "inbox"]);
}

#[test]
fn the_required_host_fields_are_all_present_and_non_empty() {
    // Herdr requires `id`, `name`, `version`, and `min_herdr_version`, and
    // the marketplace indexes only repositories whose manifest parses with
    // them. A missing one is not a degraded listing; it is no listing.
    let manifest = manifest();

    for (field, value) in [
        ("id", &manifest.id),
        ("name", &manifest.name),
        ("version", &manifest.version),
        ("min_herdr_version", &manifest.min_herdr_version),
    ] {
        assert!(!value.is_empty(), "`{field}` is required");
    }

    assert_eq!(manifest.version, env!("CARGO_PKG_VERSION"));
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
        assert_eq!(entry.id, *name);
    }
}

#[test]
fn every_subscribed_event_is_one_the_hook_reacts_to() {
    // The other half of the same property: a registration the hook ignores
    // spawns a process at the host's rate to do nothing.
    let manifest = manifest();

    assert_eq!(manifest.events.len(), crate::event::HostEvent::ALL.len());

    for entry in &manifest.events {
        let event = crate::event::HostEvent::parse(&entry.on).unwrap_or_else(|| {
            panic!(
                "the manifest subscribes to `{}`, which the hook ignores",
                entry.on
            )
        });
        assert_eq!(event.reaction(), crate::event::Reaction::RefreshStatus);
    }
}

#[test]
fn entry_identifiers_are_unique_within_their_kind() {
    let manifest = manifest();

    for (kind, mut ids) in [
        (
            "action",
            manifest
                .actions
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
        ),
        (
            "pane",
            manifest
                .panes
                .iter()
                .map(|entry| entry.id.as_str())
                .collect::<Vec<_>>(),
        ),
    ] {
        let before = ids.len();
        ids.sort_unstable();
        ids.dedup();

        assert_eq!(ids.len(), before, "two {kind} entries share an identifier");
    }
}

#[test]
fn every_platform_is_one_herdr_names() {
    let manifest = manifest();
    let known = ["linux", "macos", "windows"];

    for platform in &manifest.platforms {
        assert!(known.contains(&platform.as_str()), "unknown `{platform}`");
    }

    let covered: Vec<&str> = manifest
        .build
        .iter()
        .filter_map(|step| step.platforms.as_ref())
        .flatten()
        .map(String::as_str)
        .collect();

    for platform in &manifest.platforms {
        assert!(
            covered.contains(&platform.as_str()),
            "`{platform}` is declared supported but no build step installs the binary there"
        );
    }
}

#[test]
fn the_manifest_renders_as_toml_with_the_host_tables() {
    let rendered = manifest().to_toml();

    for expected in [
        "id = \"herdr-remote-channel\"",
        "min_herdr_version",
        "[[build]]",
        "[[startup]]",
        "[[actions]]",
        "[[events]]",
        "[[panes]]",
        "on = \"workspace.focused\"",
        "placement = \"split\"",
    ] {
        assert!(
            rendered.contains(expected),
            "the generated manifest is missing `{expected}`:\n{rendered}"
        );
    }

    assert!(
        rendered.starts_with("# Generated by"),
        "the file should say it is generated"
    );
}
