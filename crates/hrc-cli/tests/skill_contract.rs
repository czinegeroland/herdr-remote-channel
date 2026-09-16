//! The agent skill must stay in step with the CLI it documents.
//!
//! A skill file is the one artifact that ships instructions to an agent, and
//! nothing else in the build would notice if it drifted from the exit codes,
//! the command surface, or the prohibitions in PRD section 24.2. These tests
//! are that noticing.
//!
//! They deliberately check for the *substance* of each rule rather than exact
//! prose, so the wording can improve without the tests becoming a
//! transcription exercise.

use std::path::PathBuf;

/// The path PRD section 24 requires.
fn skill_path() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../.agents/skills/herdr-remote-channel/SKILL.md")
}

fn skill() -> String {
    // Git may check the file out with CRLF endings, so normalize: line
    // endings are a property of the checkout, not of the skill's content.
    std::fs::read_to_string(skill_path())
        .expect("the skill file should exist at the PRD path")
        .replace("\r\n", "\n")
}

#[test]
fn the_skill_exists_where_the_prd_requires() {
    assert!(
        skill_path().is_file(),
        "PRD section 24 requires .agents/skills/herdr-remote-channel/SKILL.md"
    );
}

#[test]
fn the_skill_has_usable_frontmatter() {
    // Without a name and a description, a skill cannot be selected, so the
    // rest of the file never gets read.
    let text = skill();

    assert!(text.starts_with("---\n"), "frontmatter must come first");
    let frontmatter = text
        .split("\n---\n")
        .next()
        .expect("frontmatter should be delimited");

    assert!(frontmatter.contains("name: herdr-remote-channel"));
    assert!(
        frontmatter.contains("description:"),
        "a skill needs a description to be selected"
    );
    assert!(
        frontmatter.contains("Use when"),
        "the description should say when to use the skill"
    );
    assert!(
        frontmatter.to_lowercase().contains("do not use"),
        "the description should say when not to"
    );
}

#[test]
fn every_exit_code_the_cli_produces_is_documented() {
    // An agent branches on these numbers. A code the skill does not mention
    // is a code an agent will mishandle.
    let text = skill();

    for code in ["0", "1", "2", "3", "4"] {
        assert!(
            text.contains(&format!("| {code} |")),
            "exit code {code} is missing from the skill's table"
        );
    }

    assert!(text.contains("authorization"), "code 4 needs explaining");
    assert!(text.contains("Not implemented"), "code 3 needs explaining");
}

#[test]
fn the_skill_states_every_prohibition_from_the_prd() {
    // PRD section 24.2. Each entry is checked by the concept it names, so
    // rewording the file does not break the test but dropping a rule does.
    let text = skill().to_lowercase();

    let prohibitions = [
        ("private keys", vec!["private key"]),
        ("invite secrets", vec!["invite secret"]),
        ("safety phrases", vec!["safety phrase"]),
        (
            "approving joins",
            vec!["approve a join", "approving a join"],
        ),
        (
            "granting capabilities",
            vec!["grant a capability", "capability"],
        ),
        ("making repositories public", vec!["public"]),
        (
            "sending sensitive content",
            vec!["sensitive context", "sensitive content"],
        ),
        (
            "delivering unapproved content",
            vec!["unapproved remote content", "unapproved"],
        ),
        ("rewriting history", vec!["force-push", "rewrite"]),
    ];

    for (rule, markers) in prohibitions {
        assert!(
            markers.iter().any(|marker| text.contains(marker)),
            "the skill does not address the prohibition on {rule}"
        );
    }
}

#[test]
fn the_skill_frames_inbound_content_as_untrusted() {
    // The single most important instruction in the file: remote content is
    // data, and a prompt-injection attempt is exactly the case where an
    // agent needs to have been told in advance.
    let text = skill();

    assert!(
        text.contains("[REMOTE HRC MESSAGE]"),
        "the skill should name the provenance banner an agent will see"
    );

    let lower = text.to_lowercase();
    assert!(lower.contains("never as directions") || lower.contains("never as instructions"));
    assert!(
        lower.contains("ignore this file"),
        "the skill should anticipate content that tries to override it"
    );
}

#[test]
fn the_skill_tells_an_agent_not_to_route_around_refusals() {
    // Exit code 4 is the security boundary. An agent that treats it as a
    // puzzle defeats the gate more effectively than an attacker could.
    let lower = skill().to_lowercase();

    assert!(
        lower.contains("never suggest a workaround") || lower.contains("do not work around"),
        "the skill should forbid routing around a refusal"
    );
    assert!(
        lower.contains("halted"),
        "the skill should cover a halted channel"
    );
    assert!(
        lower.contains("do not retry, re-clone, reset, or force-push"),
        "the skill should say what not to do about a halt"
    );
}

#[test]
fn the_skill_documents_only_commands_the_cli_actually_has() {
    // A skill that invents a command teaches an agent to fail, and it fails
    // in the worst way: the agent quotes the skill confidently and the CLI
    // rejects it.
    //
    // This checked the first word after `hrc` and nothing else, so it passed
    // while the skill documented `hrc create <name> --remote <git-url>` and
    // `hrc invite --principal <name>` — neither of which parses. `create`
    // takes `--repo <owner/name>` and has no positional name, and `invite`
    // requires a subcommand. A real agent read both and stopped, which is the
    // outcome this file exists to prevent.
    //
    // So every documented invocation is now resolved against the help of the
    // subcommand it names: each flag must exist there, and a command group
    // must be given one of its subcommands. Help is used rather than running
    // the commands, because a test that executes what a skill documents would
    // create identities and publish to transports.
    let text = skill();

    let help_for = |chain: &[String]| -> String {
        let output = std::process::Command::new(env!("CARGO_BIN_EXE_hrc"))
            .args(chain)
            .arg("--help")
            .output()
            .expect("the binary should run");
        String::from_utf8(output.stdout).expect("help should be UTF-8")
    };

    let mut checked = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("hrc ") else {
            continue;
        };

        // A trailing `# ...` is prose about the command, not part of it.
        let invocation = rest.split('#').next().unwrap_or(rest).trim();
        let tokens: Vec<&str> = invocation.split_whitespace().collect();
        if tokens.is_empty() || tokens[0].starts_with('-') {
            continue;
        }

        // Walk as far down the subcommand tree as the line actually goes. A
        // bare word is a subcommand only while the current help lists it;
        // anything else is a placeholder or an argument value.
        let mut chain: Vec<String> = Vec::new();
        let mut help = help_for(&chain);
        for token in &tokens {
            if token.starts_with('-') || token.starts_with('<') {
                break;
            }
            let candidate = format!("  {token}");
            if !help.contains(&candidate) {
                break;
            }
            chain.push((*token).to_string());
            help = help_for(&chain);
        }

        assert!(
            !chain.is_empty(),
            "the skill uses `hrc {invocation}`, whose command the CLI does not define"
        );

        // A group such as `hrc invite` cannot be run on its own. Clap spells
        // that in the usage line, so a skill that stops at the group is
        // teaching an agent a usage error.
        if help.contains("<COMMAND>") {
            assert!(
                tokens.len() > chain.len(),
                "the skill uses `hrc {invocation}`, but `hrc {}` needs a subcommand",
                chain.join(" ")
            );
        }

        for token in &tokens[chain.len()..] {
            let Some(flag) = token.strip_prefix("--") else {
                continue;
            };
            let flag = flag.split('=').next().unwrap_or(flag);
            if flag.is_empty() {
                continue;
            }
            assert!(
                help.contains(&format!("--{flag}")),
                "the skill uses `--{flag}` with `hrc {}`, which does not accept it",
                chain.join(" ")
            );
        }

        checked += 1;
    }

    assert!(checked >= 8, "expected the skill to show real commands");
}

#[test]
fn the_skill_names_every_pane_the_plugin_installs() {
    // Seven panes shipped before the skill mentioned any of them, so an agent
    // reading this file could not know they existed and fell back to telling
    // the user to open a terminal — the exact thing the panes were built to
    // remove. That is the same failure as a pane with no action to open it,
    // one layer up: built, installed, and unreachable because nothing said so.
    //
    // The manifest is the authority on what is installed, so a pane added
    // there and not described here fails this.
    let skill = skill();
    let manifest = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../herdr-plugin.toml"),
    )
    .expect("the generated plugin manifest should be readable");

    let mut titles: Vec<&str> = manifest
        .lines()
        .filter_map(|line| line.trim().strip_prefix("title = \""))
        .filter_map(|rest| rest.strip_suffix('"'))
        .collect();
    titles.sort_unstable();
    titles.dedup();

    assert!(!titles.is_empty(), "the manifest should declare titles");

    for title in titles {
        assert!(
            skill.contains(title),
            "the plugin installs `{title}`, which the skill never names, so an \
             agent cannot send anyone to it"
        );
    }
}

#[test]
fn the_skill_gathers_what_it_needs_instead_of_delegating_the_work() {
    // The user's standing requirement is that installing the plugin is
    // enough. An agent that answers "here are the commands, run them
    // yourself" has handed the work back, and the first real session did
    // exactly that.
    let skill = skill().to_lowercase();

    assert!(
        skill.contains("ask for everything you need"),
        "the skill should tell the agent to gather its inputs and proceed"
    );
    assert!(
        skill.contains("never ask for the key store passphrase"),
        "the one input the agent must not gather should be named"
    );
}

#[test]
fn the_skill_contains_no_secret_material() {
    // A skill file is copied into agent contexts and often into other
    // repositories. Nothing resembling a credential belongs in it.
    let text = skill();

    for marker in [
        "AGE-SECRET-KEY",
        "-----BEGIN",
        "ghp_",
        "AKIA",
        "HRC_PASSPHRASE=",
    ] {
        assert!(
            !text.contains(marker),
            "the skill contains something resembling a secret: {marker}"
        );
    }
}

#[test]
fn the_skill_covers_every_responsibility_the_prd_assigns_it() {
    // PRD section 24.1. A skill that omits one of these leaves an agent to
    // improvise in exactly the area the section was written to constrain.
    let lower = skill().to_lowercase();

    let responsibilities = [
        ("checking --help first", vec!["hrc --help"]),
        ("repository and channel creation", vec!["hrc create"]),
        ("invitation and joining", vec!["hrc invite", "hrc join"]),
        ("drafting notes and questions", vec!["hrc send", "hrc ask"]),
        ("task requests", vec!["hrc delegate"]),
        ("context-package drafts", vec!["context package"]),
        ("reading approved inbox content", vec!["hrc inbox"]),
        ("waiting for replies", vec!["hrc wait"]),
        ("diagnosing failures", vec!["hrc doctor"]),
        ("the receiving prompt gate", vec!["approv"]),
    ];

    for (responsibility, markers) in responsibilities {
        assert!(
            markers.iter().all(|marker| lower.contains(marker)),
            "the skill does not cover {responsibility}"
        );
    }
}

#[test]
fn the_skill_keeps_invite_secrets_out_of_machine_readable_output() {
    // HRC-SKILL-005. An invite in `--json` ends up in a transcript, and a
    // transcript is not a channel the user chose.
    let lower = skill().to_lowercase();

    assert!(lower.contains("invite secret"));
    assert!(
        lower.contains("--json") && lower.contains("never put it in"),
        "the skill should forbid emitting an invite through machine-readable output"
    );
}

#[test]
fn the_skill_limits_agents_to_context_drafts() {
    // HRC-SKILL-006 and HRC-SKILL-008: a manifest can be drafted by an
    // agent, but reviewing disclosure and authorizing the send are decisions
    // for the local human.
    let lower = skill().to_lowercase();

    assert!(lower.contains("hrc context draft"));
    assert!(lower.contains("do not run `hrc context preview`"));
    assert!(lower.contains("do not run `hrc context send`"));
    assert!(lower.contains("human-controlled"));
    assert!(lower.contains("one-use authorization"));
    assert!(lower.contains("copy a draft digest"));
    assert!(lower.contains("ignored-path") || lower.contains("ignored path"));
}
