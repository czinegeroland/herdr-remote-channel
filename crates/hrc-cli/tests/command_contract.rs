//! Command contract tests for the `hrc` executable.
//!
//! These cover the two properties the CLI already promises: the published
//! command surface of PRD section 22 exists, and the human authorization
//! boundary of PRD section 22.7 refuses agent-safe and non-interactive
//! callers instead of quietly doing the work.

use assert_cmd::Command;
use hrc_core::rpc::{AgentRequest, Request, TrustedRequest};
use hrc_ipc::{
    Client,
    endpoint::{Endpoint, Interface},
};
use serde_json::Value;
use std::io::{BufRead, BufReader, Read};

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
fn audit_reports_a_stable_json_shape() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let output = hrc_in(&home)
        .args(["audit", "--json"])
        .output()
        .expect("audit should run");
    assert!(output.status.success());

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["status"], "ok");
    assert!(value["entries"].is_array());
}

#[test]
fn sync_once_reports_a_stable_json_shape() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let output = hrc_in(&home)
        .args(["sync", "--once", "--json"])
        .output()
        .expect("sync should run");
    assert!(output.status.success());

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["status"], "ok");
    assert!(value["syncedAt"].is_string());
    assert!(value["channels"].is_array());
}

#[test]
fn daemon_reports_a_stable_startup_json_shape() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let mut child = hrc_std_in(&home)
        .args(["daemon", "--json"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("daemon should start");

    std::thread::sleep(std::time::Duration::from_millis(250));
    child.kill().expect("daemon should be killable in the test");

    let mut stdout = String::new();
    child
        .stdout
        .take()
        .expect("stdout piped")
        .read_to_string(&mut stdout)
        .expect("stdout should be readable");
    let _ = child.wait();

    let value: Value = serde_json::from_str(stdout.trim()).expect("stdout should be JSON");
    assert!(matches!(value["status"].as_str(), Some("ok" | "degraded")));
    assert!(value["syncedAt"].is_string());
    assert!(value["nextPollSeconds"].is_u64());
    assert!(value["channels"].is_array());
}

#[test]
fn daemon_serves_agent_safe_status_over_local_ipc() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let (mut child, _) = start_daemon(&home);
    let endpoint = daemon_endpoint(&home, Interface::AgentSafe);
    let runtime = daemon_runtime();
    let response: Value = runtime.block_on(async move {
        let mut client =
            Client::connect_with_retry(&endpoint, 40, std::time::Duration::from_millis(25))
                .await
                .expect("agent endpoint should come up");
        client
            .call(&Request::Agent(AgentRequest::Status))
            .await
            .expect("status call should succeed")
    });

    assert_eq!(response["status"], "ok");
    assert!(response["response"]["Status"]["channels"].is_array());

    child.kill().expect("daemon should be killable in the test");
    let _ = child.wait();
}

#[test]
fn agent_safe_daemon_endpoint_refuses_trusted_requests() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let (mut child, _) = start_daemon(&home);
    let endpoint = daemon_endpoint(&home, Interface::AgentSafe);
    let runtime = daemon_runtime();
    let response: Value = runtime.block_on(async move {
        let mut client =
            Client::connect_with_retry(&endpoint, 40, std::time::Duration::from_millis(25))
                .await
                .expect("agent endpoint should come up");
        client
            .call(&Request::Trusted(TrustedRequest::PreviewPending {
                message_id: "message-1".into(),
            }))
            .await
            .expect("trusted call should return a response")
    });

    assert_eq!(response["status"], "error");
    assert_eq!(response["code"], "authorization_required");

    child.kill().expect("daemon should be killable in the test");
    let _ = child.wait();
}

#[test]
fn daemon_binds_the_trusted_endpoint_separately() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("state");

    hrc_in(&home).arg("init").assert().success();

    let (mut child, _) = start_daemon(&home);
    let endpoint = daemon_endpoint(&home, Interface::TrustedHuman);
    let runtime = daemon_runtime();
    let response: Value = runtime.block_on(async move {
        let mut client =
            Client::connect_with_retry(&endpoint, 40, std::time::Duration::from_millis(25))
                .await
                .expect("trusted endpoint should come up");
        client
            .call(&TrustedRequest::PreviewPending {
                message_id: "message-1".into(),
            })
            .await
            .expect("trusted call should return a response")
    });

    assert_eq!(response["status"], "error");
    assert_eq!(response["code"], "unimplemented");

    child.kill().expect("daemon should be killable in the test");
    let _ = child.wait();
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
    // Private creation is ordinary work. Reaching the key store rather than
    // the authorization boundary is the property under test; without an
    // initialized installation it stops at the missing passphrase.
    let output = hrc()
        .args(["create", "--repo", "owner/channel", "--json"])
        .env_remove("HRC_PASSPHRASE")
        .output()
        .expect("command should run");

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_ne!(
        value["code"], "authorization_required",
        "private creation must not cross the section 22.7 boundary"
    );
    assert_eq!(value["code"], "no_passphrase");
}

#[test]
fn a_channel_is_created_published_and_registered() {
    // The whole path: two keys, a signed certificate, a signed genesis, a
    // real Git publication, and a local record — in that order, because a
    // channel registered locally but never published would be one this
    // installation believes in and nobody else can see.
    let home = tempfile::tempdir().expect("temporary home");
    let remote = tempfile::tempdir().expect("temporary remote");

    std::process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(remote.path())
        .output()
        .expect("git should create a bare repository");

    hrc_in(home.path()).arg("init").assert().success();

    let output = hrc_in(home.path())
        .args(["create", "--repo"])
        .arg(remote.path())
        .arg("--json")
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "create failed: {}",
        String::from_utf8_lossy(&output.stdout)
    );

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    let channel_id = value["channelId"].as_str().expect("a channel id");
    assert_eq!(channel_id.len(), 64, "the channel id is a sha256 digest");
    assert_ne!(value["principalId"], value["deviceId"]);

    // The channel is visible locally...
    let channels = hrc_in(home.path())
        .args(["channels", "--json"])
        .output()
        .expect("command should run");
    let channels: Value = serde_json::from_slice(&channels.stdout).expect("stdout should be JSON");
    assert_eq!(channels["channels"].as_array().unwrap().len(), 1);

    // ...and the genesis really reached the remote.
    let branches = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["ls-tree", "-r", "--name-only", "hrc"])
        .output()
        .expect("git should list the published tree");
    let listing = String::from_utf8_lossy(&branches.stdout);

    assert!(listing.contains("protocol.json"), "{listing}");
    assert!(listing.contains("control/log/00000000-"), "{listing}");
}

#[test]
fn creating_the_same_channel_twice_is_refused() {
    let home = tempfile::tempdir().expect("temporary home");
    let remote = tempfile::tempdir().expect("temporary remote");

    std::process::Command::new("git")
        .args(["init", "--bare", "--initial-branch=main"])
        .arg(remote.path())
        .output()
        .expect("git should create a bare repository");

    hrc_in(home.path()).arg("init").assert().success();
    hrc_in(home.path())
        .args(["create", "--repo"])
        .arg(remote.path())
        .assert()
        .success();

    // The second attempt builds a different genesis — a fresh nonce and a
    // later timestamp — so it is refused by the transport rather than by the
    // local record: the remote already has a channel on that branch.
    let output = hrc_in(home.path())
        .args(["create", "--repo"])
        .arg(remote.path())
        .arg("--json")
        .output()
        .expect("command should run");

    assert!(!output.status.success());
}

#[test]
fn init_creates_separate_principal_and_device_identities() {
    // PRD requirement HRC-CH-006. One key signs for the machine, the other
    // vouches for which machines belong to the person; a single key could
    // not distinguish the two claims.
    let home = tempfile::tempdir().expect("temporary home");

    let output = hrc_in(home.path())
        .args(["init", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    let principal = value["principalKey"].as_str().expect("a principal key");
    let device = value["signingKey"].as_str().expect("a device key");

    assert!(!principal.is_empty());
    assert_ne!(principal, device);
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

fn hrc_std_in(home: &std::path::Path) -> std::process::Command {
    let mut command = std::process::Command::new(assert_cmd::cargo::cargo_bin("hrc"));
    command
        .env("HRC_HOME", home)
        .env("HRC_PASSPHRASE", "correct horse battery staple");
    command
}

fn start_daemon(home: &std::path::Path) -> (std::process::Child, Value) {
    let mut child = hrc_std_in(home)
        .args(["daemon", "--json"])
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("daemon should start");

    let mut startup = String::new();
    let stdout = child.stdout.take().expect("stdout piped");
    let mut reader = BufReader::new(stdout);
    reader
        .read_line(&mut startup)
        .expect("daemon startup line should be readable");

    let value = serde_json::from_str(startup.trim()).expect("startup line should be JSON");
    (child, value)
}

fn daemon_endpoint(home: &std::path::Path, interface: Interface) -> Endpoint {
    Endpoint::new(&home.join("run"), interface).expect("endpoint should resolve")
}

fn daemon_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime should build")
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
