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
    std::fs::read_to_string(skill_path()).expect("the skill file should exist at the PRD path")
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
    // A skill that invents a command teaches an agent to fail. Every `hrc`
    // invocation the file shows must parse.
    let text = skill();
    let help = std::process::Command::new(env!("CARGO_BIN_EXE_hrc"))
        .arg("--help")
        .output()
        .expect("the binary should run");
    let help = String::from_utf8(help.stdout).expect("help should be UTF-8");

    let mut checked = 0;
    for line in text.lines() {
        let trimmed = line.trim();
        let Some(rest) = trimmed.strip_prefix("hrc ") else {
            continue;
        };

        let Some(command) = rest.split_whitespace().next() else {
            continue;
        };
        if command.starts_with('-') {
            continue;
        }

        assert!(
            help.contains(command),
            "the skill uses `hrc {command}`, which the CLI does not define"
        );
        checked += 1;
    }

    assert!(checked >= 8, "expected the skill to show real commands");
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
