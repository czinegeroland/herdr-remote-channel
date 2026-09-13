//! Command contract tests for the `hrc` executable.
//!
//! These cover the two properties the CLI already promises: the published
//! command surface of PRD section 22 exists, and the human authorization
//! boundary of PRD section 22.7 refuses agent-safe and non-interactive
//! callers instead of quietly doing the work.

use assert_cmd::Command;
use serde_json::Value;

/// Documented exit codes, mirrored from `crates/hrc-cli/src/exit.rs`. A test
/// that reads the constant from the binary would not notice a value change,
/// so the expected numbers are written out here on purpose.
const USAGE: i32 = 2;
const UNIMPLEMENTED: i32 = 3;
const AUTHORIZATION_REQUIRED: i32 = 4;

fn hrc() -> Command {
    Command::cargo_bin("hrc").expect("the hrc binary should build")
}

#[test]
fn help_lists_the_published_command_surface() {
    let output = hrc().arg("--help").output().expect("help should run");
    assert!(output.status.success());
    let help = String::from_utf8(output.stdout).expect("help should be UTF-8");

    for command in [
        "init", "whoami", "create", "channels", "status", "doctor", "rollover", "invite", "join",
        "members", "member", "device", "send", "ask", "reply", "delegate", "inbox", "show",
        "thread", "wait", "review", "approve", "sync", "daemon", "audit", "herdr",
    ] {
        assert!(
            help.contains(command),
            "`hrc --help` does not mention `{command}`"
        );
    }
}

#[test]
fn version_is_reported() {
    hrc().arg("--version").assert().success();
}

#[test]
fn unknown_commands_are_a_usage_error() {
    hrc().arg("teleport").assert().code(USAGE);
}

#[test]
fn conflicting_inbox_filters_are_a_usage_error() {
    hrc()
        .args(["inbox", "--unread", "--pending"])
        .assert()
        .code(USAGE);
}

#[test]
fn unimplemented_commands_report_a_stable_json_shape() {
    let output = hrc()
        .args(["inbox", "--json"])
        .output()
        .expect("inbox should run");
    assert_eq!(output.status.code(), Some(UNIMPLEMENTED));

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "unimplemented");
    assert_eq!(value["command"], "inbox");
    assert!(value["milestone"].is_string());
    assert!(value["message"].is_string());
}

#[test]
fn trusted_interface_commands_refuse_json() {
    for command in ["review", "approve"] {
        let output = hrc()
            .args([command, "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--json"])
            .output()
            .expect("command should run");

        assert_eq!(
            output.status.code(),
            Some(USAGE),
            "`hrc {command} --json` should be a usage error"
        );
        assert!(
            output.stdout.is_empty(),
            "`hrc {command} --json` must not write to standard output"
        );
        let stderr = String::from_utf8(output.stderr).expect("stderr should be UTF-8");
        assert!(
            stderr.contains("--json"),
            "the error should name the rejected option"
        );
    }
}

#[test]
fn trusted_interface_commands_require_local_authorization() {
    for command in ["review", "approve"] {
        let output = hrc()
            .args([command, "01ARZ3NDEKTSV4RRFFQ69G5FAV"])
            .output()
            .expect("command should run");

        assert_eq!(output.status.code(), Some(AUTHORIZATION_REQUIRED));
        assert!(
            output.stdout.is_empty(),
            "no content may reach standard output"
        );
    }
}

#[test]
fn human_authorization_boundary_commands_are_refused() {
    let boundary: [&[&str]; 6] = [
        &["join", "approve", "join-request-1"],
        &["join", "reject", "join-request-1"],
        &["member", "remove", "principal-1"],
        &["device", "revoke", "device-1"],
        &[
            "create",
            "--repo",
            "owner/channel",
            "--visibility",
            "public",
        ],
        &["rollover"],
    ];

    for args in boundary {
        let output = hrc().args(args).output().expect("command should run");
        assert_eq!(
            output.status.code(),
            Some(AUTHORIZATION_REQUIRED),
            "`hrc {}` should require trusted human authorization",
            args.join(" ")
        );
    }
}

#[test]
fn boundary_commands_report_authorization_required_in_json() {
    let output = hrc()
        .args(["member", "remove", "principal-1", "--json"])
        .output()
        .expect("command should run");

    assert_eq!(output.status.code(), Some(AUTHORIZATION_REQUIRED));
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "authorization_required");
    assert_eq!(value["command"], "member remove");
}

#[test]
fn private_channel_creation_stays_on_the_agent_safe_path() {
    hrc()
        .args(["create", "--repo", "owner/channel"])
        .assert()
        .code(UNIMPLEMENTED);
}

#[test]
fn herdr_entry_points_are_dispatched_by_the_same_binary() {
    for args in [
        vec!["herdr", "startup"],
        vec!["herdr", "action", "inbox"],
        vec!["herdr", "event"],
        vec!["herdr", "pane", "inbox"],
    ] {
        hrc().args(&args).assert().code(UNIMPLEMENTED);
    }
}

#[test]
fn an_invite_code_and_the_join_subcommands_both_parse() {
    hrc()
        .args(["join", "hrc1-invite-code"])
        .assert()
        .code(UNIMPLEMENTED);
    hrc().args(["join", "pending"]).assert().code(UNIMPLEMENTED);
}

/// Runs `hrc` against a private state directory, so a test never touches the
/// developer's real installation. The child process gets its own
/// environment; this test process never mutates its own.
fn hrc_in(home: &std::path::Path) -> Command {
    let mut command = hrc();
    command
        .env("HRC_HOME", home)
        .env("HRC_PASSPHRASE", "correct horse battery staple");
    command
}

#[test]
fn init_then_whoami_reports_a_stable_identity() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    let init = hrc_in(&home)
        .args(["init", "--json"])
        .output()
        .expect("init should run");
    assert!(init.status.success(), "init failed: {init:?}");

    let created: Value = serde_json::from_slice(&init.stdout).expect("init prints JSON");
    assert_eq!(created["status"], "ok");

    let whoami = hrc_in(&home)
        .args(["whoami", "--json"])
        .output()
        .expect("whoami should run");
    assert!(whoami.status.success());

    let reported: Value = serde_json::from_slice(&whoami.stdout).expect("whoami prints JSON");
    assert_eq!(created["signingKey"], reported["signingKey"]);
}

#[test]
fn a_command_needing_keys_without_a_passphrase_is_a_usage_error() {
    // Failing with a clear instruction beats prompting in a context that may
    // have no terminal, such as the daemon or a CI job.
    let directory = tempfile::tempdir().unwrap();

    let output = hrc()
        .env("HRC_HOME", directory.path())
        .env_remove("HRC_PASSPHRASE")
        .args(["whoami", "--json"])
        .output()
        .expect("whoami should run");

    assert_eq!(output.status.code(), Some(USAGE));

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["code"], "no_passphrase");
    assert!(
        value["message"]
            .as_str()
            .unwrap()
            .contains("HRC_PASSPHRASE"),
        "the error should name the variable that fixes it"
    );
}

#[test]
fn init_is_refused_a_second_time() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let output = hrc_in(&home)
        .args(["init", "--json"])
        .output()
        .expect("init should run");

    assert_eq!(output.status.code(), Some(USAGE));
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["code"], "already_initialized");
}

#[test]
fn doctor_reports_healthy_after_init() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let output = hrc_in(&home)
        .args(["doctor", "--json"])
        .output()
        .expect("doctor should run");
    assert!(output.status.success());

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["healthy"], true);
}

#[test]
fn human_output_is_rendered_without_json_syntax() {
    // PRD section 27 keeps machine-readable output separate from human
    // formatting. A human running `hrc status` should not be shown JSON.
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let output = hrc_in(&home).arg("status").output().expect("status runs");
    let text = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert!(output.status.success());
    assert!(text.contains("state directory"), "got: {text}");
    assert!(
        !text.contains('{'),
        "human output should not be JSON: {text}"
    );
}

#[test]
fn a_command_never_prints_the_passphrase_or_a_private_key() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    let init = hrc_in(&home).arg("init").output().expect("init runs");
    let whoami = hrc_in(&home).arg("whoami").output().expect("whoami runs");
    let doctor = hrc_in(&home).arg("doctor").output().expect("doctor runs");

    for output in [init, whoami, doctor] {
        let combined = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        assert!(!combined.contains("AGE-SECRET-KEY"), "leaked a private key");
        assert!(
            !combined.contains("correct horse battery staple"),
            "leaked the passphrase"
        );
    }
}
