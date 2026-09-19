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
            Some("node"),
            "`{command:?}` must run the launcher through node"
        );
        assert_eq!(
            command.get(1).map(String::as_str),
            Some(LAUNCHER),
            "`{command:?}` must run the plugin's own launcher"
        );
        assert_eq!(
            command.get(2).map(String::as_str),
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
        LAUNCHER.contains('/'),
        "`{LAUNCHER}` would be looked up on PATH"
    );
    assert!(
        !LAUNCHER.starts_with('/'),
        "`{LAUNCHER}` must be relative to the plugin root"
    );
}

#[test]
fn installing_the_plugin_needs_no_compiler() {
    // The whole point of publishing the executable to npm was that nobody
    // should need a toolchain. Building from source here contradicted that,
    // and failed on the first real Windows install: the MSVC target wants
    // Visual Studio Build Tools, a multi-gigabyte prerequisite to read an
    // inbox pane. A source build belongs in `herdr plugin link`, not in
    // `herdr plugin install`.
    for step in &manifest().build {
        let command = step.command.join(" ");
        for compiler in ["cargo", "rustc", "rustup", "make", "cc", "gcc"] {
            assert!(
                !step.command.iter().any(|argument| argument == compiler),
                "build step `{command}` needs a toolchain"
            );
        }
        assert!(
            step.command.iter().any(|argument| argument == "npm")
                || step.command.iter().any(|argument| argument == "npx"),
            "build step `{command}` should install the executable or the skill"
        );
    }
}

#[test]
fn the_installed_version_is_pinned_to_this_build() {
    // A floating `latest` would let Herdr read one version's manifest and
    // install a different version's executable, so the entry points it
    // registered and the binary answering them could disagree.
    let expected = format!("{PACKAGE}@{}", env!("CARGO_PKG_VERSION"));

    for step in &manifest().build {
        // The skill comes from a repository rather than a registry, and
        // `skills add` takes no tag or commit, so only the executable step
        // has a version to pin (decision DEC-079).
        if step.command.iter().any(|argument| argument == "skills") {
            continue;
        }
        assert!(
            step.command.contains(&expected),
            "build step `{}` should pin `{expected}`",
            step.command.join(" ")
        );
    }
}

#[test]
fn installing_the_plugin_installs_the_agent_skill() {
    // Installing the plugin used to leave the panes reachable and the agent
    // unable to drive the CLI: the skill was a separate `npx skills add` the
    // user had to know about, so "install the plugin and it works" was true
    // of the panes and false of everything an agent does. A platform that
    // installs the executable and not the skill is half a plugin.
    let manifest = manifest();

    for platform in &manifest.platforms {
        let installs = manifest
            .build
            .iter()
            .filter(|step| step.command.iter().any(|argument| argument == "skills"))
            .filter(|step| match &step.platforms {
                Some(platforms) => platforms.contains(platform),
                None => true,
            })
            .count();

        assert_eq!(
            installs, 1,
            "`{platform}` should install the agent skill exactly once"
        );
    }

    let step = manifest
        .build
        .iter()
        .find(|step| step.command.iter().any(|argument| argument == "skills"))
        .expect("a skill install step");

    // A build step has nobody to answer a prompt, and a plugin install has no
    // business writing a skill into every agent on the machine.
    for argument in ["--yes", "--global", "--agent", "claude-code"] {
        assert!(
            step.command.iter().any(|entry| entry == argument),
            "the skill install should pass `{argument}`: {:?}",
            step.command
        );
    }
}

#[test]
fn every_platform_the_plugin_claims_has_a_build_step() {
    // A platform listed with no step that applies to it installs nothing and
    // then fails at the first entry point, which is a worse failure than
    // refusing to install.
    let manifest = manifest();

    for platform in &manifest.platforms {
        assert!(
            manifest.build.iter().any(|step| match &step.platforms {
                Some(platforms) => platforms.contains(platform),
                None => true,
            }),
            "`{platform}` is claimed but nothing installs the executable there"
        );
    }
}

#[test]
fn the_manifest_declares_the_four_entry_points_the_prd_lists() {
    // PRD section 13.2: startup, action, event, and pane.
    let manifest = manifest();

    // `node`, then the launcher path, then the `hrc herdr ...` arguments.
    let tail = |command: &[String]| command[2..].to_vec();

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

    // A step with no `platforms` runs everywhere, which is the whole point of
    // the build being `cargo` and nothing else. This assertion used to require
    // each platform to be named by some step, which was right while there was
    // one script per platform and is the wrong question now: what matters is
    // that every platform a person can install on has something that installs
    // there, however that coverage is expressed.
    let covers_everything = manifest.build.iter().any(|step| step.platforms.is_none());

    let named: Vec<&str> = manifest
        .build
        .iter()
        .filter_map(|step| step.platforms.as_ref())
        .flatten()
        .map(String::as_str)
        .collect();

    for platform in &manifest.platforms {
        assert!(
            covers_everything || named.contains(&platform.as_str()),
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

#[test]
fn opening_the_review_popup_names_a_pane_the_manifest_registers() {
    // A pane Herdr does not know about is a command that fails at the moment
    // a person presses Enter on a message. Holding the argv to the manifest
    // means the two cannot drift.
    let argv = open_review("01ARZ3NDEKTSV4RRFFQ69G5FAV");
    let manifest = manifest();

    let plugin = position(&argv, "--plugin");
    assert_eq!(argv[plugin + 1], manifest.id);

    let entrypoint = position(&argv, "--entrypoint");
    assert!(
        manifest
            .panes
            .iter()
            .any(|pane| pane.id == argv[entrypoint + 1]),
        "`{}` is not a registered pane",
        argv[entrypoint + 1]
    );

    // The popup placement is what makes the revealed body session-modal.
    // Opening the review pane as a split would leave a decrypted message
    // sitting in the tiled workspace.
    let placement = position(&argv, "--placement");
    assert_eq!(argv[placement + 1], "popup");
    assert_eq!(
        manifest
            .panes
            .iter()
            .find(|pane| pane.id == argv[entrypoint + 1])
            .map(|pane| pane.placement.as_str()),
        Some("popup"),
        "the manifest and the open request must agree about modality"
    );
}

#[test]
fn the_review_popup_is_handed_an_identifier_and_nothing_else() {
    // Everything on this command line is fixed text except the identifier,
    // and the caller has already established that it is a ULID. Nothing a
    // sender wrote can reach the host through here.
    let argv = open_review("01ARZ3NDEKTSV4RRFFQ69G5FAV");

    let environment = position(&argv, "--env");
    assert_eq!(
        argv[environment + 1],
        format!("{REVIEW_TARGET_ENV}=01ARZ3NDEKTSV4RRFFQ69G5FAV")
    );

    assert_eq!(
        argv.iter()
            .filter(|argument| argument.starts_with(REVIEW_TARGET_ENV))
            .count(),
        1,
        "exactly one target reaches the popup"
    );

    // The fallback carries no target at all, rather than an empty one that
    // the popup would have to decide how to read.
    assert!(
        !open_review_list()
            .iter()
            .any(|argument| argument.contains(REVIEW_TARGET_ENV))
    );
}

/// Where one flag sits in an argv, so the test reads the value beside it
/// rather than a hard-coded index that a reordering would silently break.
fn position(argv: &[String], flag: &str) -> usize {
    argv.iter()
        .position(|argument| argument == flag)
        .unwrap_or_else(|| panic!("`{flag}` is missing from {argv:?}"))
}
