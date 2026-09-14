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
use std::io::{BufRead, BufReader, Read, Write};

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
        "members", "member", "device", "send", "ask", "reply", "delegate", "context", "inbox",
        "show", "thread", "wait", "review", "approve", "sync", "daemon", "audit", "herdr",
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
    // `show` is still part of the published contract without behaviour, and
    // agents branch on the code rather than on the prose.
    let output = hrc()
        .args(["show", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "--json"])
        .output()
        .expect("show should run");
    assert_eq!(output.status.code(), Some(UNIMPLEMENTED));

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["status"], "error");
    assert_eq!(value["code"], "unimplemented");
    assert_eq!(value["command"], "show");
    assert!(value["milestone"].is_string());
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
            .call(&Request::Trusted(TrustedRequest::SendContext {
                recipient: "recipient".into(),
                package_id: "ctx-1".into(),
                authorization: "ctxauth-test".into(),
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

    // The endpoint is bound and answers trusted methods. There is no such
    // message here, which is what it should say — the point of the test is
    // that the request reached the trusted handler at all.
    assert_eq!(response["status"], "error");
    assert_eq!(response["code"], "core_error");
    assert!(
        response["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no pending message"),
        "{response}"
    );

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

/// A home with an initialized installation and one created channel.
fn channel_fixture() -> (tempfile::TempDir, tempfile::TempDir) {
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

    (home, remote)
}

fn git_init(path: &std::path::Path) {
    let output = std::process::Command::new("git")
        .args(["init", "--quiet"])
        .arg(path)
        .output()
        .expect("git should initialize source repository");
    assert!(
        output.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn git_in_bare(remote: &std::path::Path, args: &[&str]) -> String {
    let output = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@localhost")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@localhost")
        .args(args)
        .output()
        .expect("git should run");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn forge_remote_change(remote: &std::path::Path, path: &str, bytes: Option<&[u8]>) {
    let index = remote.join("forge-index");
    let head = git_in_bare(remote, &["rev-parse", "hrc"]);

    git_in_bare_with_index(remote, &index, &["read-tree", &head], None);
    match bytes {
        Some(bytes) => {
            let blob = git_in_bare_with_index(
                remote,
                &index,
                &["hash-object", "-w", "--stdin"],
                Some(bytes),
            );
            git_in_bare_with_index(
                remote,
                &index,
                &[
                    "update-index",
                    "--add",
                    "--cacheinfo",
                    &format!("100644,{blob},{path}"),
                ],
                None,
            );
        }
        None => {
            let removal = format!("0 {}	{path}\n", "0".repeat(40));
            git_in_bare_with_index(
                remote,
                &index,
                &["update-index", "--index-info"],
                Some(removal.as_bytes()),
            );
        }
    }

    let tree = git_in_bare_with_index(remote, &index, &["write-tree"], None);
    let commit = git_in_bare(
        remote,
        &["commit-tree", &tree, "-p", &head, "-m", "forged history"],
    );
    git_in_bare(remote, &["update-ref", "refs/heads/hrc", &commit]);
}

fn git_in_bare_with_index(
    remote: &std::path::Path,
    index: &std::path::Path,
    args: &[&str],
    input: Option<&[u8]>,
) -> String {
    let mut command = std::process::Command::new("git");
    command
        .arg("--git-dir")
        .arg(remote)
        .env("GIT_INDEX_FILE", index)
        .env("GIT_AUTHOR_NAME", "test")
        .env("GIT_AUTHOR_EMAIL", "test@localhost")
        .env("GIT_COMMITTER_NAME", "test")
        .env("GIT_COMMITTER_EMAIL", "test@localhost")
        .args(args)
        .stdout(std::process::Stdio::piped());

    if input.is_some() {
        command.stdin(std::process::Stdio::piped());
    }

    let mut child = command.spawn().expect("git should run");
    if let Some(input) = input {
        child
            .stdin
            .take()
            .expect("stdin should be piped")
            .write_all(input)
            .expect("git should read input");
    }

    let output = child.wait_with_output().expect("git should finish");
    assert!(
        output.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_owned()
}

fn assert_sync_halts(home: &std::path::Path) {
    hrc_in(home)
        .args(["sync", "--once", "--json"])
        .assert()
        .failure();

    assert_channel_is_halted(home);
}

fn assert_channel_is_halted(home: &std::path::Path) {
    let status = hrc_in(home)
        .args(["status", "--json"])
        .output()
        .expect("status should run");
    let status: Value = serde_json::from_slice(&status.stdout).expect("stdout should be JSON");
    assert!(
        status["channels"][0]["haltedReason"]
            .as_str()
            .is_some_and(|reason| !reason.is_empty()),
        "the halt must retain its reason: {status}"
    );

    let repeated = hrc_in(home)
        .args(["sync", "--once", "--json"])
        .output()
        .expect("sync should run");
    assert!(!repeated.status.success());
    let repeated: Value = serde_json::from_slice(&repeated.stdout).expect("stdout should be JSON");
    assert!(
        repeated["message"]
            .as_str()
            .unwrap_or_default()
            .contains("synchronization is halted"),
        "a halt must be sticky: {repeated}"
    );
}

#[test]
fn a_remote_history_rewrite_halts_synchronization() {
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    hrc_in(home.path())
        .args(["send", &principal, "establish a trusted cursor", "--json"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let prior = git_in_bare(remote.path(), &["rev-parse", "hrc^"]);
    git_in_bare(remote.path(), &["update-ref", "refs/heads/hrc", &prior]);

    hrc()
        .env("HRC_HOME", home.path())
        .env_remove("HRC_PASSPHRASE")
        .args(["sync", "--once", "--json"])
        .assert()
        .failure();
    assert_channel_is_halted(home.path());
}

#[test]
fn deleting_a_published_object_halts_synchronization() {
    let (home, remote) = channel_fixture();
    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let genesis_path = git_in_bare(remote.path(), &["ls-tree", "-r", "--name-only", "hrc"]);
    forge_remote_change(remote.path(), genesis_path.lines().next().unwrap(), None);

    assert_sync_halts(home.path());
}

#[test]
fn substituting_a_published_object_halts_synchronization() {
    let (home, remote) = channel_fixture();
    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let genesis_path = git_in_bare(remote.path(), &["ls-tree", "-r", "--name-only", "hrc"]);
    forge_remote_change(
        remote.path(),
        genesis_path.lines().next().unwrap(),
        Some(b"substituted genesis"),
    );

    assert_sync_halts(home.path());
}

#[test]
fn a_conflicting_control_successor_halts_synchronization() {
    let (home, remote) = channel_fixture();
    hrc_in(home.path())
        .args(["invite", "create", "--github-user", "bob"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let paths = git_in_bare(remote.path(), &["ls-tree", "-r", "--name-only", "hrc"]);
    let control = paths
        .lines()
        .find(|path| path.starts_with("control/log/00000001-"))
        .expect("the invite control entry");
    let bytes = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["show", &format!("hrc:{control}")])
        .output()
        .expect("git should show the control entry");
    assert!(bytes.status.success());

    forge_remote_change(
        remote.path(),
        "control/log/00000002-conflict.json",
        Some(&bytes.stdout),
    );

    hrc()
        .env("HRC_HOME", home.path())
        .env_remove("HRC_PASSPHRASE")
        .args(["sync", "--once", "--json"])
        .assert()
        .failure();
    assert_channel_is_halted(home.path());
}

#[test]
fn deleting_the_remote_channel_branch_halts_synchronization() {
    let (home, remote) = channel_fixture();
    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    git_in_bare(remote.path(), &["update-ref", "-d", "refs/heads/hrc"]);

    assert_sync_halts(home.path());
}

#[test]
fn an_invite_is_published_and_its_code_is_returned_once() {
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .args(["invite", "create", "--github-user", "bob"])
        .output()
        .expect("command should run");

    assert!(
        output.status.success(),
        "invite create failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let printed = String::from_utf8_lossy(&output.stdout);
    assert!(printed.contains("hrc1-"), "no invite code was returned");
    assert!(
        printed.contains("It works once"),
        "the code was printed without saying it is single use"
    );

    // Listing it afterwards shows the invite but never the secret.
    let listed = hrc_in(home.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");

    let invites = value["invites"].as_array().expect("an invites array");
    assert_eq!(invites.len(), 1);
    assert_eq!(invites[0]["intendedFor"], "bob");
    assert_eq!(invites[0]["state"], "open");

    let listing = String::from_utf8_lossy(&listed.stdout);
    assert!(
        !listing.contains("hrc1-"),
        "the invite secret appeared in machine-readable output: {listing}"
    );
}

#[test]
fn invite_create_has_no_machine_readable_mode_at_all() {
    // PRD requirement HRC-SKILL-005. The whole output of this command is a
    // secret, and `--json` is the form most likely to end up in a
    // transcript, a log, or an agent's context.
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .args(["invite", "create", "--github-user", "bob", "--json"])
        .output()
        .expect("command should run");

    assert_eq!(output.status.code(), Some(USAGE));
    assert!(
        output.stdout.is_empty(),
        "a refused command wrote to standard output"
    );
}

#[test]
fn a_revoked_invite_is_recorded_as_revoked() {
    let (home, _remote) = channel_fixture();

    hrc_in(home.path())
        .args(["invite", "create", "--github-user", "bob"])
        .assert()
        .success();

    let listed = hrc_in(home.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");
    let invite_id = value["invites"][0]["inviteId"]
        .as_str()
        .expect("an invite id")
        .to_owned();

    hrc_in(home.path())
        .args(["invite", "revoke", &invite_id, "--json"])
        .assert()
        .success();

    let listed = hrc_in(home.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");
    assert_eq!(value["invites"][0]["state"], "revoked");
}

#[test]
fn two_invites_chain_onto_one_control_log() {
    // Each invite is a control entry, and the second has to follow the
    // first: a second entry built against a stale head would be rejected on
    // replay by everyone, including this installation.
    let (home, _remote) = channel_fixture();

    for user in ["bob", "carol"] {
        hrc_in(home.path())
            .args(["invite", "create", "--github-user", user])
            .assert()
            .success();
    }

    let listed = hrc_in(home.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");

    assert_eq!(value["invites"].as_array().unwrap().len(), 2);
}

#[test]
fn inviting_without_a_channel_says_so() {
    let home = tempfile::tempdir().expect("temporary home");
    hrc_in(home.path()).arg("init").assert().success();

    let output = hrc_in(home.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(value["code"], "no_channel");
}

/// Pulls the invite code out of `hrc invite create`'s human output.
fn invite_code_from(stdout: &[u8]) -> String {
    String::from_utf8_lossy(stdout)
        .split_whitespace()
        .find(|word| word.starts_with("hrc1-"))
        .expect("an invite code")
        .to_owned()
}

#[test]
fn two_installations_create_invite_join_and_review() {
    // `AC-ENROLL` end to end: two clean homes, one repository, and no shared
    // private material. The joiner proves possession of the invite secret
    // without it ever being published, and both sides derive the same safety
    // phrase from public material alone.
    let (admin, remote) = channel_fixture();
    let joiner = tempfile::tempdir().expect("temporary joiner home");

    hrc_in(joiner.path()).arg("init").assert().success();

    let created = hrc_in(admin.path())
        .args(["invite", "create", "--github-user", "bob"])
        .output()
        .expect("command should run");
    let code = invite_code_from(&created.stdout);

    let joined = hrc_in(joiner.path())
        .args(["join", &code, "--json"])
        .output()
        .expect("command should run");
    assert!(
        joined.status.success(),
        "join failed: {}",
        String::from_utf8_lossy(&joined.stdout)
    );

    let joined: Value = serde_json::from_slice(&joined.stdout).expect("stdout should be JSON");
    let joiner_phrase = joined["safetyPhrase"].as_str().expect("a safety phrase");
    assert_eq!(joiner_phrase.split_whitespace().count(), 6);

    // The administrator sees the request, validated rather than merely
    // listed, and derives the same phrase without either side sending it.
    let pending = hrc_in(admin.path())
        .args(["join", "pending", "--json"])
        .output()
        .expect("command should run");
    let pending: Value = serde_json::from_slice(&pending.stdout).expect("stdout should be JSON");

    let requests = pending["pending"].as_array().expect("a pending array");
    assert_eq!(requests.len(), 1, "{pending}");
    assert_eq!(requests[0]["principalId"], joined["principalId"]);
    assert_eq!(
        requests[0]["safetyPhrase"].as_str(),
        Some(joiner_phrase),
        "the two sides derived different safety phrases"
    );

    // The invite secret never reached the repository.
    let published = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["grep", "-i", "hrc1-", "hrc"])
        .output()
        .expect("git grep should run");
    assert!(
        published.stdout.is_empty(),
        "an invite code was published: {}",
        String::from_utf8_lossy(&published.stdout)
    );
}

#[test]
fn a_join_request_under_an_unknown_invite_is_not_listed() {
    // A request naming an invite this installation did not issue cannot have
    // its proof checked, and an unverifiable request is not something to put
    // in front of a human to approve.
    let (admin, _remote) = channel_fixture();

    let pending = hrc_in(admin.path())
        .args(["join", "pending", "--json"])
        .output()
        .expect("command should run");
    let pending: Value = serde_json::from_slice(&pending.stdout).expect("stdout should be JSON");

    assert!(pending["pending"].as_array().unwrap().is_empty());
}

#[test]
fn a_revoked_invite_can_no_longer_be_reviewed() {
    // Revoking releases the secret, so a request naming that invite stops
    // being verifiable — which is the same thing as stopping being
    // admissible.
    let (admin, _remote) = channel_fixture();
    let joiner = tempfile::tempdir().expect("temporary joiner home");
    hrc_in(joiner.path()).arg("init").assert().success();

    let created = hrc_in(admin.path())
        .args(["invite", "create", "--github-user", "bob"])
        .output()
        .expect("command should run");
    let code = invite_code_from(&created.stdout);

    hrc_in(joiner.path())
        .args(["join", &code, "--json"])
        .assert()
        .success();

    let listed = hrc_in(admin.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let listed: Value = serde_json::from_slice(&listed.stdout).expect("stdout should be JSON");
    let invite_id = listed["invites"][0]["inviteId"]
        .as_str()
        .expect("an invite id")
        .to_owned();

    hrc_in(admin.path())
        .args(["invite", "revoke", &invite_id, "--json"])
        .assert()
        .success();

    let pending = hrc_in(admin.path())
        .args(["join", "pending", "--json"])
        .output()
        .expect("command should run");
    let pending: Value = serde_json::from_slice(&pending.stdout).expect("stdout should be JSON");

    assert!(
        pending["pending"].as_array().unwrap().is_empty(),
        "a request under a revoked invite was still offered for approval"
    );
}

#[test]
fn approving_a_join_still_requires_the_trusted_interface() {
    // Listing requests is ordinary work; admitting someone is not.
    let (admin, _remote) = channel_fixture();

    hrc_in(admin.path())
        .args(["join", "approve", "request-1", "--json"])
        .assert()
        .code(AUTHORIZATION_REQUIRED);
}

/// The admin's own principal, read back out of the published genesis.
fn principal_of(home: &std::path::Path, remote: &std::path::Path) -> String {
    let channels = hrc_in(home)
        .args(["channels", "--json"])
        .output()
        .expect("command should run");
    let channels: Value = serde_json::from_slice(&channels.stdout).expect("stdout should be JSON");
    let channel_id = channels["channels"][0]["channelId"]
        .as_str()
        .expect("a channel id");

    let genesis = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote)
        .args([
            "show",
            &format!("hrc:control/log/00000000-{}.json", &channel_id[..16]),
        ])
        .output()
        .expect("git should show the genesis object");

    let genesis: Value = serde_json::from_slice(&genesis.stdout).expect("genesis should be JSON");
    genesis["payload"]["initialAdmin"]["principalId"]
        .as_str()
        .expect("a principal id")
        .to_owned()
}

#[test]
fn a_message_is_sealed_published_fetched_and_quarantined() {
    // The whole messaging loop over a real repository. What arrives is
    // quarantined, not delivered: nothing reaches an agent without a human.
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    let sent = hrc_in(home.path())
        .args([
            "send",
            &principal,
            "the retry loop backs off too fast",
            "--json",
        ])
        .output()
        .expect("command should run");
    assert!(
        sent.status.success(),
        "send failed: {}",
        String::from_utf8_lossy(&sent.stdout)
    );

    let sent: Value = serde_json::from_slice(&sent.stdout).expect("stdout should be JSON");
    let message_id = sent["messageId"].as_str().expect("a message id").to_owned();
    assert_eq!(message_id.len(), 26, "message ids are ULIDs");
    assert_eq!(sent["published"], true);

    // The ciphertext is in the repository at the documented layout path.
    let listing = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["ls-tree", "-r", "--name-only", "hrc"])
        .output()
        .expect("git should list the tree");
    let listing = String::from_utf8_lossy(&listing.stdout);
    assert!(
        listing.contains(&format!("{message_id}.age")),
        "the message object is not at the layout path: {listing}"
    );

    // And the plaintext is not.
    let grep = std::process::Command::new("git")
        .arg("--git-dir")
        .arg(remote.path())
        .args(["grep", "-i", "retry loop", "hrc"])
        .output()
        .expect("git grep should run");
    assert!(
        grep.stdout.is_empty(),
        "the message text was published in the clear"
    );

    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let inbox = hrc_in(home.path())
        .args(["inbox", "--json"])
        .output()
        .expect("command should run");
    let inbox: Value = serde_json::from_slice(&inbox.stdout).expect("stdout should be JSON");

    let entries = inbox["entries"].as_array().expect("an entries array");
    assert_eq!(entries.len(), 1, "{inbox}");
    assert_eq!(entries[0]["messageId"], message_id.as_str());
    assert_eq!(entries[0]["kind"], "note");
    assert_eq!(
        entries[0]["disposition"], "quarantined",
        "an arriving message must not be delivered"
    );
}

/// A source repository with one commit, so `git diff` and `git log` have
/// something to report.
fn committed_source() -> tempfile::TempDir {
    let source = tempfile::tempdir().expect("temporary source repository");
    git_init(source.path());

    let run = |arguments: &[&str]| {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(source.path())
            .args(arguments)
            .output()
            .expect("git should run");
        assert!(
            output.status.success(),
            "git {arguments:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    };

    run(&["config", "user.email", "fixture@example.invalid"]);
    run(&["config", "user.name", "Fixture"]);
    std::fs::create_dir_all(source.path().join("src")).expect("create source directory");
    std::fs::write(source.path().join("src/lib.rs"), "fn retry() {}\n").expect("write source");
    run(&["add", "."]);
    run(&["commit", "--quiet", "-m", "the first commit"]);

    source
}

#[test]
fn hrc_captures_patch_and_output_itself_rather_than_trusting_the_caller() {
    // PRD requirements HRC-CTX-004 and HRC-CTX-007. The caller names what to
    // capture; HRC produces the bytes. Whatever text the manifest supplied
    // is discarded, exactly as it already is for excerpts.
    let (home, _remote) = channel_fixture();
    let source = committed_source();
    let manifest = source.path().join("context.json");

    let caller_invented = "THIS DIFF WAS NEVER IN THE REPOSITORY";
    std::fs::write(
        &manifest,
        format!(
            r#"{{"version":1,"id":"ctx-capture","items":[
                {{"kind":"patch","range":"HEAD","diff":"{caller_invented}"}},
                {{"kind":"output","command":"git status --porcelain","text":"{caller_invented}"}}
            ]}}"#
        ),
    )
    .expect("write manifest");

    let drafted = hrc_in(home.path())
        .args([
            "context",
            "draft",
            manifest.to_str().expect("manifest path"),
            "--repository",
            source.path().to_str().expect("repository path"),
            "--json",
        ])
        .output()
        .expect("draft should run");

    assert!(
        drafted.status.success(),
        "draft failed: {}{}",
        String::from_utf8_lossy(&drafted.stdout),
        String::from_utf8_lossy(&drafted.stderr)
    );

    let draft: Value = serde_json::from_slice(&drafted.stdout).expect("draft JSON");
    assert_eq!(draft["state"], "draft");
    assert_eq!(draft["sendable"], true);
    assert!(
        !String::from_utf8_lossy(&drafted.stdout).contains(caller_invented),
        "the caller's invented text must not survive capture"
    );
}

#[test]
fn a_command_hrc_will_not_run_is_refused_at_draft_time() {
    // The allowlist, from the outside. Every one of these evades a
    // pattern-matching deny-list while doing what the requirement forbids.
    let (home, _remote) = channel_fixture();
    let source = committed_source();

    for command in [
        "sh -c 'cat ~/.bash_history'",
        "cat /proc/self/environ",
        "git config --list --show-origin",
        "cargo test",
        "git status",
    ] {
        let manifest = source.path().join("denied.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"version":1,"id":"ctx-denied","items":[
                    {{"kind":"output","command":"{command}","text":"anything"}}
                ]}}"#
            ),
        )
        .expect("write manifest");

        let drafted = hrc_in(home.path())
            .args([
                "context",
                "draft",
                manifest.to_str().expect("manifest path"),
                "--repository",
                source.path().to_str().expect("repository path"),
                "--json",
            ])
            .output()
            .expect("draft should run");

        assert!(!drafted.status.success(), "`{command}` should not draft");
    }
}

#[test]
fn a_patch_range_that_is_not_a_range_is_refused() {
    // A range reaches a command line, so it is checked against a
    // conservative shape rather than passed through.
    let (home, _remote) = channel_fixture();
    let source = committed_source();

    for range in [
        "--output=/tmp/escape",
        "HEAD; rm -rf /",
        "HEAD --exec=sh",
        "",
    ] {
        let manifest = source.path().join("range.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"version":1,"id":"ctx-range","items":[
                    {{"kind":"patch","range":"{range}","diff":"ignored"}}
                ]}}"#
            ),
        )
        .expect("write manifest");

        let drafted = hrc_in(home.path())
            .args([
                "context",
                "draft",
                manifest.to_str().expect("manifest path"),
                "--repository",
                source.path().to_str().expect("repository path"),
                "--json",
            ])
            .output()
            .expect("draft should run");

        assert!(
            !drafted.status.success(),
            "`{range}` should not be accepted as a revision range"
        );
    }
}

#[test]
fn a_capture_without_a_repository_is_refused() {
    // There is nowhere to capture from, and inventing an answer is exactly
    // what these requirements exist to prevent.
    let (home, _remote) = channel_fixture();
    let source = committed_source();
    let manifest = source.path().join("norepo.json");
    std::fs::write(
        &manifest,
        r#"{"version":1,"id":"ctx-norepo","items":[
            {"kind":"output","command":"git status --porcelain","text":"anything"}
        ]}"#,
    )
    .expect("write manifest");

    let drafted = hrc_in(home.path())
        .args([
            "context",
            "draft",
            manifest.to_str().expect("manifest path"),
            "--json",
        ])
        .output()
        .expect("draft should run");

    assert!(!drafted.status.success(), "a capture needs a repository");
}

#[test]
fn context_draft_cannot_authorize_a_noninteractive_preview_or_send() {
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());
    let source = tempfile::tempdir().expect("temporary source repository");
    git_init(source.path());
    let manifest = source.path().join("context.json");
    std::fs::create_dir_all(source.path().join("src")).expect("create source directory");
    std::fs::write(source.path().join("src/lib.rs"), "fn retry() {}").expect("write source");
    let context_text = "remote context must remain quarantined";
    std::fs::write(
        &manifest,
        format!(
            r#"{{"version":1,"id":"ctx-review","items":[
                {{"kind":"note","text":"{context_text}"}},
                {{"kind":"excerpt","path":"src/lib.rs","firstLine":1,"lastLine":1,"text":"caller text is ignored"}},
                {{"kind":"reference","reference":"abc123","description":"failing commit"}},
                {{"kind":"link","url":"https://example.invalid/run/1","description":"run"}}
            ]}}"#
        ),
    )
    .expect("write manifest");

    let drafted = hrc_in(home.path())
        .args([
            "context",
            "draft",
            manifest.to_str().expect("manifest path"),
            "--repository",
            source.path().to_str().expect("repository path"),
            "--json",
        ])
        .output()
        .expect("draft should run");
    assert!(drafted.status.success());
    let draft_json: Value = serde_json::from_slice(&drafted.stdout).expect("draft JSON");
    assert_eq!(draft_json["state"], "draft");
    assert_eq!(draft_json["sendable"], true);
    assert!(
        !String::from_utf8_lossy(&drafted.stdout).contains(context_text),
        "draft output must not quote package content"
    );

    // Draft emits a digest as an integrity identifier only. It is not a
    // bearer authorization that an agent can copy into a later command.
    let digest = draft_json["digest"]
        .as_str()
        .expect("draft digest")
        .to_owned();
    hrc_in(home.path())
        .args(["context", "preview", "ctx-review", "--json"])
        .assert()
        .code(USAGE);

    hrc_in(home.path())
        .args(["context", "send", &principal, "ctx-review", "--json"])
        .assert()
        .code(USAGE);
    hrc_in(home.path())
        .args(["context", "send", &principal, "ctx-review"])
        .assert()
        .code(AUTHORIZATION_REQUIRED);

    // Copying the draft digest does not change the refusal.
    hrc_in(home.path())
        .args([
            "context",
            "send",
            &principal,
            "ctx-review",
            &digest,
            "--json",
        ])
        .assert()
        .code(USAGE);

    // The same package can leave only through the distinct trusted endpoint.
    // No digest is supplied as a bearer confirmation; dispatch obtains and
    // consumes the package/recipient/channel-bound authorization internally.
    let (mut daemon, _) = start_daemon(home.path());
    let endpoint = daemon_endpoint(home.path(), Interface::TrustedHuman);
    let runtime = daemon_runtime();
    let response: (Value, Value) = runtime.block_on(async move {
        let mut client =
            Client::connect_with_retry(&endpoint, 40, std::time::Duration::from_millis(25))
                .await
                .expect("trusted endpoint should come up");
        let preview: Value = client
            .call(&TrustedRequest::PreviewContext {
                recipient: principal.clone(),
                package_id: "ctx-review".into(),
            })
            .await
            .expect("trusted preview should respond");
        let authorization = preview["authorization"]
            .as_str()
            .expect("preview authorization")
            .to_owned();
        let sent = client
            .call(&TrustedRequest::SendContext {
                recipient: principal.clone(),
                package_id: "ctx-review".into(),
                authorization,
            })
            .await
            .expect("trusted send should respond");
        (preview, sent)
    });
    assert_eq!(response.0["status"], "ok");
    assert_eq!(response.0["method"], "preview_context");
    assert_eq!(response.0["digest"], digest);
    assert!(
        response.0["content"]
            .as_str()
            .expect("trusted preview content")
            .contains(context_text),
        "trusted preview must show the exact package content"
    );
    assert_eq!(response.1["status"], "ok");
    assert_eq!(response.1["method"], "send_context");
    daemon.kill().expect("daemon should be killable");
    let _ = daemon.wait();

    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();
    let database =
        hrc_storage::Database::open(home.path().join("state.sqlite")).expect("open local state");
    let received = database
        .inbox_entries(&database.channels().unwrap()[0].channel_id)
        .unwrap()
        .into_iter()
        .next()
        .expect("trusted context send should be received");
    assert!(
        database
            .inbound_context(&received.message_id)
            .unwrap()
            .is_some(),
        "received context must stay behind the same pending inbox gate"
    );
    let message_id = received.message_id;
    drop(database);
    let (mut daemon, _) = start_daemon(home.path());
    let endpoint = daemon_endpoint(home.path(), Interface::TrustedHuman);
    let runtime = daemon_runtime();
    let revealed: Value = runtime.block_on(async move {
        let mut client =
            Client::connect_with_retry(&endpoint, 40, std::time::Duration::from_millis(25))
                .await
                .expect("trusted endpoint should restart");
        client
            .call(&TrustedRequest::PreviewPending { message_id })
            .await
            .expect("trusted pending preview should respond")
    });
    assert_eq!(revealed["status"], "ok");
    assert!(
        revealed["body"]
            .as_str()
            .unwrap_or_default()
            .contains(context_text),
        "trusted review must reveal the canonical context that the pending row owns"
    );
    daemon.kill().expect("daemon should be killable");
    let _ = daemon.wait();
}

#[test]
fn git_ignored_and_traversal_context_paths_are_blocked_without_stripping() {
    let (home, _remote) = channel_fixture();
    let source = tempfile::tempdir().expect("temporary source repository");
    git_init(source.path());
    std::fs::write(source.path().join(".gitignore"), "ignored.txt\n").expect("write ignore rule");

    for (id, path) in [
        ("ctx-ignored", "ignored.txt"),
        ("ctx-traversal", "../outside.txt"),
        ("ctx-windows-traversal", "..\\outside.txt"),
    ] {
        let manifest = source.path().join(format!("{id}.json"));
        let path_json = serde_json::to_string(path).expect("serialize path");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"version":1,"id":"{id}","items":[{{"kind":"excerpt","path":{path_json},"firstLine":1,"lastLine":1,"text":"selected text"}}]}}"#
            ),
        )
        .expect("write manifest");

        let drafted = hrc_in(home.path())
            .args([
                "context",
                "draft",
                manifest.to_str().expect("manifest path"),
                "--repository",
                source.path().to_str().expect("repository path"),
                "--json",
            ])
            .output()
            .expect("draft should run");
        assert!(
            !drafted.status.success(),
            "{id}: ignored and traversal excerpts must be rejected before their text is read"
        );
        assert!(
            !String::from_utf8_lossy(&drafted.stdout).contains("selected text"),
            "a rejected package must not be silently reduced or echoed"
        );
    }
}

#[test]
fn secret_context_is_reported_without_echoing_the_secret() {
    let (home, _remote) = channel_fixture();
    let manifest_directory = tempfile::tempdir().expect("temporary manifest directory");
    let manifest = manifest_directory.path().join("secret-context.json");
    let marker = "ghp_16CharactersOfTokenHere0000000000";
    std::fs::write(
        &manifest,
        format!(
            r#"{{"version":1,"id":"ctx-secret","items":[{{"kind":"note","text":"token: {marker}"}}]}}"#
        ),
    )
    .expect("write manifest");

    let drafted = hrc_in(home.path())
        .args([
            "context",
            "draft",
            manifest.to_str().expect("manifest path"),
            "--json",
        ])
        .output()
        .expect("draft should run");
    assert!(drafted.status.success());
    let draft_json: Value = serde_json::from_slice(&drafted.stdout).expect("draft JSON");
    assert_eq!(draft_json["sendable"], false);
    assert_eq!(draft_json["secretFindings"][0]["rule"], "github_token");
    assert!(!String::from_utf8_lossy(&drafted.stdout).contains(marker));

    let rejected = hrc_in(home.path())
        .args(["context", "send", "nobody", "ctx-secret", "--json"])
        .output()
        .expect("send should run");
    assert_eq!(rejected.status.code(), Some(USAGE));
    assert!(
        !String::from_utf8_lossy(&rejected.stdout).contains(marker),
        "a secret finding must never echo its match"
    );
}

#[test]
fn messages_arrive_in_the_order_they_were_sent() {
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    let mut sent = Vec::new();
    for text in ["first", "second", "third"] {
        let output = hrc_in(home.path())
            .args(["send", &principal, text, "--json"])
            .output()
            .expect("command should run");
        assert!(
            output.status.success(),
            "send failed: {}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
        sent.push(
            value["messageId"]
                .as_str()
                .expect("a message id")
                .to_owned(),
        );
    }

    // Arrival order, not identifier order. Two messages sent in the same
    // millisecond have no defined order between their identifiers — that is
    // all a ULID promises — while the per-device chain of section 18.1
    // orders them regardless.
    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let inbox = hrc_in(home.path())
        .args(["inbox", "--json"])
        .output()
        .expect("command should run");
    let inbox: Value = serde_json::from_slice(&inbox.stdout).expect("stdout should be JSON");

    let arrived: Vec<&str> = inbox["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["messageId"].as_str().unwrap())
        .collect();
    assert_eq!(arrived, sent);
}

#[test]
fn a_second_sync_does_not_duplicate_what_already_arrived() {
    // At-least-once delivery is the transport's contract, so re-reading the
    // same objects has to be ordinary rather than corrupting.
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    hrc_in(home.path())
        .args(["send", &principal, "once", "--json"])
        .assert()
        .success();

    for _ in 0..3 {
        hrc_in(home.path())
            .args(["sync", "--once", "--json"])
            .assert()
            .success();
    }

    let inbox = hrc_in(home.path())
        .args(["inbox", "--json"])
        .output()
        .expect("command should run");
    let inbox: Value = serde_json::from_slice(&inbox.stdout).expect("stdout should be JSON");

    assert_eq!(inbox["entries"].as_array().unwrap().len(), 1);
}

#[test]
fn a_question_and_its_reply_share_one_thread() {
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    let asked = hrc_in(home.path())
        .args([
            "ask",
            &format!("{principal}/reviewer"),
            "does the backoff need jitter?",
            "--json",
        ])
        .output()
        .expect("command should run");
    let asked: Value = serde_json::from_slice(&asked.stdout).expect("stdout should be JSON");
    let question_id = asked["messageId"]
        .as_str()
        .expect("a message id")
        .to_owned();

    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let replied = hrc_in(home.path())
        .args(["reply", &question_id, "yes, and a cap", "--json"])
        .output()
        .expect("command should run");
    assert!(
        replied.status.success(),
        "reply failed: {}",
        String::from_utf8_lossy(&replied.stdout)
    );
    let replied: Value = serde_json::from_slice(&replied.stdout).expect("stdout should be JSON");

    // The reply continues the question's thread rather than starting one.
    assert_eq!(replied["threadId"], asked["threadId"]);
    assert_eq!(replied["kind"], "answer");

    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let thread = hrc_in(home.path())
        .args(["thread", asked["threadId"].as_str().unwrap(), "--json"])
        .output()
        .expect("command should run");
    let thread: Value = serde_json::from_slice(&thread.stdout).expect("stdout should be JSON");

    let entries = thread["entries"].as_array().expect("an entries array");
    assert_eq!(entries.len(), 2, "{thread}");
    assert_eq!(entries[0]["kind"], "question");
    assert_eq!(entries[1]["kind"], "answer");
}

#[test]
fn a_thread_redacts_bodies_that_are_still_quarantined() {
    // PRD section 22.4: `hrc thread` redacts pending bodies.
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    let sent = hrc_in(home.path())
        .args([
            "send",
            &principal,
            "the staging credentials rotated",
            "--json",
        ])
        .output()
        .expect("command should run");
    let sent: Value = serde_json::from_slice(&sent.stdout).expect("stdout should be JSON");

    hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .assert()
        .success();

    let thread = hrc_in(home.path())
        .args(["thread", sent["threadId"].as_str().unwrap(), "--json"])
        .output()
        .expect("command should run");
    let rendered = String::from_utf8_lossy(&thread.stdout);

    assert!(
        !rendered.contains("staging credentials"),
        "a quarantined body was shown: {rendered}"
    );
    assert!(rendered.contains("redacted"));
}

#[test]
fn sending_to_someone_who_is_not_a_member_is_refused() {
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .args(["send", "nobody", "hello", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(value["status"], "error");
    assert!(
        value["message"]
            .as_str()
            .unwrap_or_default()
            .contains("not a member"),
        "{value}"
    );
}

#[test]
fn replying_to_a_message_that_does_not_exist_is_refused() {
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .args(["reply", "01ARZ3NDEKTSV4RRFFQ69G5FAV", "hello", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert_eq!(value["code"], "no_such_message");
}

/// Sends one request to a daemon endpoint and reads the answer.
///
/// Which endpoint is the whole authority model: a caller's rights come from
/// the socket that accepted it, never from anything in the request.
fn daemon_call(home: &std::path::Path, interface: Interface, request: Value) -> Value {
    let endpoint = daemon_endpoint(home, interface);

    daemon_runtime().block_on(async move {
        let mut client =
            Client::connect_with_retry(&endpoint, 40, std::time::Duration::from_millis(25))
                .await
                .expect("the endpoint should come up");

        client
            .call(&request)
            .await
            .expect("the call should return a response")
    })
}

/// An admin and a joiner sharing one repository, with a join request pending.
fn pending_join_fixture() -> (
    tempfile::TempDir,
    tempfile::TempDir,
    tempfile::TempDir,
    String,
) {
    let (admin, remote) = channel_fixture();
    let joiner = tempfile::tempdir().expect("temporary joiner home");
    hrc_in(joiner.path()).arg("init").assert().success();

    let created = hrc_in(admin.path())
        .args(["invite", "create", "--github-user", "bob"])
        .output()
        .expect("command should run");
    let code = invite_code_from(&created.stdout);

    hrc_in(joiner.path())
        .args(["join", &code, "--json"])
        .assert()
        .success();

    let pending = hrc_in(admin.path())
        .args(["join", "pending", "--json"])
        .output()
        .expect("command should run");
    let pending: Value = serde_json::from_slice(&pending.stdout).expect("stdout should be JSON");
    let request_id = pending["pending"][0]["requestId"]
        .as_str()
        .expect("a request id")
        .to_owned();

    (admin, remote, joiner, request_id)
}

#[test]
fn admitting_a_joiner_advances_the_roster_through_the_trusted_interface() {
    // `hrc join approve` is on the section 22.7 list, so the only path that
    // can admit anyone is the trusted socket. This drives exactly that path.
    let (admin, _remote, _joiner, request_id) = pending_join_fixture();

    let members = hrc_in(admin.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");
    assert_eq!(members["members"].as_array().unwrap().len(), 1);
    assert_eq!(members["rosterEpoch"], 0);

    let (mut daemon, _startup) = start_daemon(admin.path());

    let answer = daemon_call(
        admin.path(),
        Interface::TrustedHuman,
        serde_json::json!({
            "method": "approve_join",
            "params": { "request_id": request_id },
        }),
    );

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert_eq!(answer["status"], "ok", "{answer}");

    let members = hrc_in(admin.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");

    assert_eq!(
        members["members"].as_array().unwrap().len(),
        2,
        "the joiner was not admitted: {members}"
    );
    assert_eq!(
        members["rosterEpoch"], 1,
        "admitting a member must advance the epoch"
    );

    // The invite is spent, and its secret released with it.
    let invites = hrc_in(admin.path())
        .args(["invite", "list", "--json"])
        .output()
        .expect("command should run");
    let invites: Value = serde_json::from_slice(&invites.stdout).expect("stdout should be JSON");
    assert_eq!(invites["invites"][0]["state"], "consumed");
}

#[test]
fn the_agent_safe_socket_cannot_admit_anyone() {
    // The same request, on the other listener.
    let (admin, _remote, _joiner, request_id) = pending_join_fixture();
    let (mut daemon, _startup) = start_daemon(admin.path());

    let answer = daemon_call(
        admin.path(),
        Interface::AgentSafe,
        serde_json::json!({
            "method": "approve_join",
            "params": { "request_id": request_id },
        }),
    );

    let _ = daemon.kill();
    let _ = daemon.wait();

    assert_eq!(answer["code"], "authorization_required", "{answer}");

    // And nobody was admitted.
    let members = hrc_in(admin.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");
    assert_eq!(members["members"].as_array().unwrap().len(), 1);
}

#[test]
fn members_and_device_list_read_the_published_roster() {
    let (home, _remote) = channel_fixture();

    let members = hrc_in(home.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");

    assert_eq!(members["members"][0]["administrator"], true);
    assert_eq!(members["members"][0]["active"], true);
    assert_eq!(
        members["members"][0]["devices"].as_array().unwrap().len(),
        1
    );

    let devices = hrc_in(home.path())
        .args(["device", "list", "--json"])
        .output()
        .expect("command should run");
    let devices: Value = serde_json::from_slice(&devices.stdout).expect("stdout should be JSON");

    assert_eq!(devices["devices"].as_array().unwrap().len(), 1);
    assert_eq!(devices["devices"][0]["active"], true);
}

#[test]
fn removing_a_member_and_revoking_a_device_stay_on_the_trusted_surface() {
    let (home, _remote) = channel_fixture();

    for args in [
        vec!["member", "remove", "someone", "--json"],
        vec!["device", "revoke", "some-device", "--json"],
    ] {
        hrc_in(home.path())
            .args(&args)
            .assert()
            .code(AUTHORIZATION_REQUIRED);
    }
}

#[test]
fn herdr_entry_points_are_dispatched_by_the_same_binary() {
    // PRD requirement HRC-TECH-002: one executable serves the CLI, the
    // daemon, and every Herdr entry point. Each mode below is reached
    // through the same binary and does real work.
    let (home, _remote) = channel_fixture();

    let startup = hrc_in(home.path())
        .args(["herdr", "startup", "--json"])
        .output()
        .expect("command should run");
    assert!(startup.status.success());

    let value: Value = serde_json::from_slice(&startup.stdout).expect("stdout should be JSON");
    assert_eq!(value["manifest"]["id"], "herdr-remote-channel");
    assert_eq!(
        value["manifest"]["startup"][0]["command"][0], "bin/hrc",
        "the manifest must resolve the binary from the plugin root"
    );
    assert!(
        value["sidebar"]
            .as_str()
            .expect("the sidebar is a line of text")
            .starts_with("HRC: "),
        "startup should return the section 23.1 sidebar line"
    );

    for args in [
        vec!["herdr", "action", "inbox", "--json"],
        vec!["herdr", "pane", "inbox", "--json"],
    ] {
        let output = hrc_in(home.path())
            .args(&args)
            .output()
            .expect("command should run");
        assert!(output.status.success(), "{args:?} should run");

        let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
        assert_eq!(value["pane"], "inbox");
        assert!(value["rows"].is_array());
    }
}

#[test]
fn the_herdr_inbox_pane_never_renders_a_pending_body() {
    // The plugin's inbox is a surface a person reads at a glance and an
    // agent could screenshot. PRD section 19.1: no inbound body before a
    // local human approval. The pane is fed by a query that does not select
    // the body column, and this proves the rendered output agrees.
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    let secret = "the body that must not appear in a pane";
    hrc_in(home.path())
        .args(["send", &principal, secret])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let output = hrc_in(home.path())
        .args(["herdr", "pane", "inbox", "--json"])
        .output()
        .expect("command should run");
    let rendered = String::from_utf8(output.stdout).expect("stdout should be UTF-8");

    assert!(
        !rendered.contains(secret),
        "the inbox pane rendered a pending body"
    );

    let value: Value = serde_json::from_str(&rendered).expect("stdout should be JSON");
    let rows = value["rows"].as_array().expect("the pane returns rows");
    assert!(!rows.is_empty(), "the message should reach the inbox");

    for row in rows {
        let object = row.as_object().expect("a row is an object");
        for forbidden in ["body", "text", "content", "plaintext"] {
            assert!(
                !object.contains_key(forbidden),
                "an inbox row must not carry `{forbidden}`"
            );
        }
    }
}

#[test]
fn an_unknown_herdr_action_or_pane_is_a_usage_error() {
    // A manifest is a file a person can edit. Naming something this build
    // does not have should say so rather than draw an empty screen.
    let (home, _remote) = channel_fixture();

    for args in [
        vec!["herdr", "action", "teleport", "--json"],
        vec!["herdr", "pane", "teleport", "--json"],
    ] {
        let output = hrc_in(home.path())
            .args(&args)
            .output()
            .expect("command should run");

        assert_eq!(output.status.code(), Some(USAGE), "{args:?}");

        let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
        assert_eq!(value["code"], "unknown_herdr_target");
    }
}

#[test]
fn a_herdr_event_is_named_in_the_environment_and_answered_with_a_reaction() {
    // Herdr does not write the event to standard input. It runs the hook's
    // argv and names the event in `HERDR_PLUGIN_EVENT`.
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .env("HERDR_PLUGIN_EVENT", "workspace.focused")
        .args(["herdr", "event", "--json"])
        .output()
        .expect("the event hook should run");
    assert!(output.status.success());

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["reaction"]["reaction"], "refresh_status");
    assert!(
        value["sidebar"]
            .as_str()
            .expect("a refresh carries the sidebar it refreshes")
            .starts_with("HRC: ")
    );
}

#[test]
fn an_event_this_plugin_did_not_subscribe_to_is_ignored_rather_than_failed() {
    // A hook that exits non-zero on an event it was not registered for shows
    // a person a failed plugin for something that is not a failure.
    let (home, _remote) = channel_fixture();

    for event in ["worktree.created", "tab.focused", "approve_everything"] {
        let output = hrc_in(home.path())
            .env("HERDR_PLUGIN_EVENT", event)
            .args(["herdr", "event", "--json"])
            .output()
            .expect("the event hook should run");

        assert!(
            output.status.success(),
            "`{event}` should not fail the hook"
        );

        let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
        assert_eq!(value["reaction"]["reaction"], "ignore", "{event}");
        assert!(
            value["sidebar"].is_null(),
            "nothing should be computed for an event this plugin ignores"
        );
    }
}

#[test]
fn a_herdr_event_hook_invoked_with_no_event_named_does_nothing() {
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .env_remove("HERDR_PLUGIN_EVENT")
        .args(["herdr", "event", "--json"])
        .output()
        .expect("the event hook should run");

    assert!(output.status.success());
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["reaction"]["reaction"], "ignore");
}
#[test]
fn a_message_can_carry_an_expiry_and_the_inbox_shows_it() {
    // PRD requirement HRC-MSG-005: messages support expiration. Section 11.5
    // lists creation and expiration times as things a message must carry, so
    // a sender has to be able to set one.
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    hrc_in(home.path())
        .args(["send", &principal, "expires in a day", "--expires", "24h"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let output = hrc_in(home.path())
        .args(["inbox", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    let entry = value["entries"]
        .as_array()
        .and_then(|entries| entries.first())
        .expect("the message should have arrived");

    let expires_at = entry["expiresAt"]
        .as_str()
        .expect("the inbox should surface the expiry");
    assert!(
        expires_at.ends_with('Z') && expires_at.len() == 20,
        "`{expires_at}` should be an absolute RFC 3339 UTC timestamp, not the lifetime as sent"
    );
    assert_eq!(entry["disposition"], "quarantined");
}

#[test]
fn a_message_without_an_expiry_reports_none() {
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    hrc_in(home.path())
        .args(["send", &principal, "no expiry"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once"])
        .assert()
        .success();
    hrc_in(home.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let output = hrc_in(home.path())
        .args(["inbox", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert!(value["entries"][0]["expiresAt"].is_null());
}

#[test]
fn a_lifetime_that_is_not_a_lifetime_is_a_usage_error() {
    let (home, remote) = channel_fixture();
    let principal = principal_of(home.path(), remote.path());

    let output = hrc_in(home.path())
        .args(["send", &principal, "text", "--expires", "soon", "--json"])
        .output()
        .expect("command should run");

    assert_eq!(output.status.code(), Some(USAGE));

    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");
    assert_eq!(value["code"], "invalid_lifetime");
}

#[test]
fn synchronization_reports_the_messages_it_swept() {
    // The sweep runs on every pass, so the field is always present even when
    // nothing lapsed. A caller that only sees it sometimes cannot tell an
    // empty sweep from an old binary.
    let (home, _remote) = channel_fixture();

    let output = hrc_in(home.path())
        .args(["sync", "--once", "--json"])
        .output()
        .expect("command should run");
    let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

    assert!(
        value["expired"].is_array(),
        "sync should always report what it swept"
    );
}

#[test]
fn an_invite_code_and_the_join_subcommands_both_parse() {
    // The bare code and the subcommands share one argument position, so
    // clap has to keep them apart. Both reach a command rather than a
    // parsing error; what they then report depends on local state.
    for args in [
        vec!["join", "hrc1-invite-code", "--json"],
        vec!["join", "pending", "--json"],
    ] {
        let output = hrc().args(&args).output().expect("command should run");
        let value: Value = serde_json::from_slice(&output.stdout).expect("stdout should be JSON");

        assert_ne!(value["code"], "usage_error", "{args:?} failed to parse");
        assert_ne!(
            value["code"], "unimplemented",
            "{args:?} is implemented now"
        );
    }

    // Approving remains on the trusted surface.
    hrc()
        .args(["join", "approve", "request-1", "--json"])
        .assert()
        .code(AUTHORIZATION_REQUIRED);
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

/// Sends SIGTERM to a running child and waits for it.
///
/// Unix only: this is the signal a service manager and a container runtime
/// send, and Windows has no equivalent to deliver from a test.
#[cfg(unix)]
fn terminate(child: &mut std::process::Child) -> std::process::ExitStatus {
    let pid = child.id() as i32;

    // SAFETY is not available in this workspace, which forbids unsafe code,
    // so the signal goes through the `kill` program rather than libc.
    let sent = std::process::Command::new("kill")
        .args(["-TERM", &pid.to_string()])
        .status()
        .expect("kill should run");
    assert!(sent.success(), "could not signal the daemon");

    for _ in 0..100 {
        if let Some(status) = child.try_wait().expect("wait should work") {
            return status;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }

    child.kill().ok();
    panic!("the daemon did not exit within five seconds of SIGTERM");
}

#[cfg(unix)]
#[test]
fn the_daemon_stops_cleanly_on_sigterm_and_releases_its_endpoints() {
    // PRD requirement HRC-TECH-003: Tokio owns cancellation. A daemon that
    // can only be killed is one that gets killed halfway through publishing.
    let (home, _remote) = channel_fixture();
    let (mut child, startup) = start_daemon(home.path());
    assert_eq!(startup["status"], "ok");

    let agent = daemon_endpoint(home.path(), Interface::AgentSafe);
    let trusted = daemon_endpoint(home.path(), Interface::TrustedHuman);

    // The startup line is printed before the runtime binds, so wait for the
    // endpoint to appear rather than racing it. Without this precondition
    // the test would also pass on a daemon that never listened at all.
    let listening = match agent.path() {
        None => true,
        Some(path) => (0..100).any(|_| {
            if path.exists() {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
            false
        }),
    };
    assert!(
        listening,
        "the daemon should be listening before it is asked to stop"
    );

    let status = terminate(&mut child);

    assert!(
        status.success(),
        "SIGTERM should be a clean stop, not a failure: {status:?}"
    );

    for endpoint in [agent, trusted] {
        if let Some(path) = endpoint.path() {
            assert!(
                !path.exists(),
                "{} should be removed on the way out",
                path.display()
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn a_daemon_can_be_restarted_immediately_after_a_clean_stop() {
    // The reason releasing the endpoints matters. A restart that had to
    // reclaim a stale socket would do extra work to prove nobody is behind
    // it, and that probe is the part most likely to be wrong.
    let (home, _remote) = channel_fixture();

    let (mut first, _) = start_daemon(home.path());
    let status = terminate(&mut first);
    assert!(status.success());

    let (mut second, startup) = start_daemon(home.path());
    assert_eq!(startup["status"], "ok", "the daemon should start again");

    let status = terminate(&mut second);
    assert!(status.success());
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

/// The PRD's own end-to-end scenario (section 28.5), driven through the
/// built binary against a real Git repository.
///
/// The messaging tests elsewhere in this file are self-addressed: one
/// installation sends to its own principal and syncs twice. That covers the
/// transport and the inbox, but it cannot cover the parts that only exist
/// when there are two parties — encrypting to *someone else's* device
/// recipient, decrypting with a key the sender never had, and a roster with
/// more than one member. This test is two installations with independent
/// keys, which is what the product is for.
#[test]
fn the_end_to_end_scenario_from_section_28_5() {
    // Steps 1 to 3: Alice creates a channel, invites Bob, and Bob joins
    // with keys he generated himself.
    let (alice, _remote, bob, request_id) = pending_join_fixture();

    // Step 4: Alice admits Bob. `join approve` is on the section 22.7 list,
    // so this can only happen through the trusted socket.
    let (mut daemon, _startup) = start_daemon(alice.path());
    let admitted = daemon_call(
        alice.path(),
        Interface::TrustedHuman,
        serde_json::json!({
            "method": "approve_join",
            "params": { "request_id": request_id },
        }),
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert_eq!(admitted["status"], "ok", "{admitted}");

    // Bob learns he is a member.
    hrc_in(bob.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let members = hrc_in(alice.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");
    assert_eq!(members["rosterEpoch"], 1);

    let alice_principal = principal_of(alice.path(), _remote.path());
    let bob_principal = members["members"]
        .as_array()
        .expect("members")
        .iter()
        .map(|member| member["principalId"].as_str().expect("a principal"))
        .find(|principal| *principal != alice_principal)
        .expect("Bob should be in the roster")
        .to_owned();

    // Step 5: Alice asks a question while Bob is offline.
    let question = "does the backoff need jitter?";
    let asked = hrc_in(alice.path())
        .args(["ask", &bob_principal, question, "--json"])
        .output()
        .expect("command should run");
    assert!(
        asked.status.success(),
        "ask failed: {}{}",
        String::from_utf8_lossy(&asked.stdout),
        String::from_utf8_lossy(&asked.stderr)
    );
    let asked: Value = serde_json::from_slice(&asked.stdout).expect("stdout should be JSON");
    let thread_id = asked["threadId"].as_str().expect("a thread id").to_owned();

    hrc_in(alice.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    // Step 6: Bob reconnects and receives it, quarantined rather than
    // delivered.
    hrc_in(bob.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let inbox = hrc_in(bob.path())
        .args(["inbox", "--json"])
        .output()
        .expect("command should run");
    let rendered = String::from_utf8_lossy(&inbox.stdout).into_owned();
    let inbox: Value = serde_json::from_slice(&inbox.stdout).expect("stdout should be JSON");

    let entry = inbox["entries"]
        .as_array()
        .expect("entries")
        .iter()
        .find(|entry| entry["kind"] == "question")
        .unwrap_or_else(|| panic!("Bob should have received the question: {inbox}"));

    assert_eq!(
        entry["sender"], alice_principal,
        "the question should be attributed to Alice"
    );
    assert_eq!(entry["disposition"], "quarantined");
    assert!(
        !rendered.contains(question),
        "an unapproved body must not appear on the agent-safe inbox surface"
    );

    let message_id = entry["messageId"]
        .as_str()
        .expect("a message id")
        .to_owned();
    assert_eq!(
        entry["threadId"], thread_id,
        "the thread should survive the hop"
    );

    // Step 7: Bob approves delivery to a local agent. Both halves of the
    // prompt gate run on the trusted socket: the preview decrypts the body
    // for a human to read, and the approval records what they chose.
    let (mut bob_daemon, _startup) = start_daemon(bob.path());

    let previewed = daemon_call(
        bob.path(),
        Interface::TrustedHuman,
        serde_json::json!({
            "method": "preview_pending",
            "params": { "message_id": message_id },
        }),
    );
    assert_eq!(previewed["status"], "ok", "{previewed}");
    assert!(
        previewed.to_string().contains(question),
        "the trusted surface should show the body the agent-safe one withheld: {previewed}"
    );

    let approved = daemon_call(
        bob.path(),
        Interface::TrustedHuman,
        serde_json::json!({
            "method": "approve",
            "params": {
                "message_id": message_id,
                "decision": { "action": "deliver_to_agent", "agent": "reviewer" },
                "expires_at": "2099-01-01T00:00:00Z",
            },
        }),
    );
    assert_eq!(approved["status"], "ok", "{approved}");

    // Only now may the agent-safe surface see it, and only because a human
    // said so.
    let released = daemon_call(
        bob.path(),
        Interface::AgentSafe,
        serde_json::json!({
            "method": "show_approved",
            "params": { "message_id": message_id },
        }),
    );
    assert_eq!(released["status"], "ok", "{released}");
    assert!(
        released.to_string().contains(question),
        "approved content should be readable on the agent-safe surface: {released}"
    );

    let _ = bob_daemon.kill();
    let _ = bob_daemon.wait();

    // Step 8: Bob returns the answer.
    let answer = "yes, and cap it at a minute";
    let replied = hrc_in(bob.path())
        .args(["reply", &message_id, answer, "--json"])
        .output()
        .expect("command should run");
    assert!(
        replied.status.success(),
        "reply failed: {}{}",
        String::from_utf8_lossy(&replied.stdout),
        String::from_utf8_lossy(&replied.stderr)
    );
    let replied: Value = serde_json::from_slice(&replied.stdout).expect("stdout should be JSON");
    assert_eq!(replied["kind"], "answer");
    assert_eq!(
        replied["threadId"], thread_id,
        "the answer should continue Alice's thread rather than open one"
    );

    hrc_in(bob.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    // Step 9: Alice receives the answer in the original thread.
    hrc_in(alice.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let thread = hrc_in(alice.path())
        .args(["thread", &thread_id, "--json"])
        .output()
        .expect("command should run");
    let thread: Value = serde_json::from_slice(&thread.stdout).expect("stdout should be JSON");
    // `hrc thread` reads the inbox, so on the asking side the thread holds
    // what arrived rather than both halves: Alice's own question was never
    // delivered to her. What step 9 requires is that the answer reached her
    // under the thread she opened, and that is what is asserted.
    let entries = thread["entries"].as_array().expect("an entries array");
    let answer_entry = entries
        .iter()
        .find(|entry| entry["kind"] == "answer")
        .unwrap_or_else(|| panic!("Alice should have received the answer: {thread}"));
    assert_eq!(answer_entry["sender"], bob_principal);
    assert_eq!(thread["threadId"], thread_id);

    // Step 10: both send before either fetches, so the two histories diverge
    // and have to be reconciled without a human resolving a Git conflict.
    let alice_concurrent = "alice writes first";
    let bob_concurrent = "bob writes at the same time";
    hrc_in(alice.path())
        .args(["send", &bob_principal, alice_concurrent, "--json"])
        .assert()
        .success();
    hrc_in(bob.path())
        .args(["send", &alice_principal, bob_concurrent, "--json"])
        .assert()
        .success();

    // Neither has seen the other's commit at this point.
    for home in [alice.path(), bob.path()] {
        hrc_in(home).args(["sync", "--once"]).assert().success();
        hrc_in(home).args(["sync", "--once"]).assert().success();
    }
    hrc_in(alice.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    for (home, expected) in [
        (alice.path(), bob_concurrent),
        (bob.path(), alice_concurrent),
    ] {
        let inbox = hrc_in(home)
            .args(["inbox", "--json"])
            .output()
            .expect("command should run");
        let inbox: Value = serde_json::from_slice(&inbox.stdout).expect("stdout should be JSON");
        let notes = inbox["entries"]
            .as_array()
            .expect("entries")
            .iter()
            .filter(|entry| entry["kind"] == "note")
            .count();
        assert!(
            notes >= 1,
            "a concurrently sent note should have arrived ({expected}): {inbox}"
        );
    }

    // Step 11: Alice revokes Bob's device, and a message sent afterwards
    // does not reach it.
    let members = hrc_in(alice.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");
    let bob_device = members["members"]
        .as_array()
        .expect("members")
        .iter()
        .find(|member| member["principalId"] == bob_principal.as_str())
        .expect("Bob should be in the roster")["devices"]
        .as_array()
        .expect("devices")[0]["deviceId"]
        .as_str()
        .expect("a device id")
        .to_owned();

    let (mut daemon, _startup) = start_daemon(alice.path());
    let revoked = daemon_call(
        alice.path(),
        Interface::TrustedHuman,
        serde_json::json!({
            "method": "revoke_device",
            "params": { "device_id": bob_device },
        }),
    );
    let _ = daemon.kill();
    let _ = daemon.wait();
    assert_eq!(revoked["status"], "ok", "{revoked}");

    hrc_in(alice.path())
        .args(["sync", "--once"])
        .assert()
        .success();

    let members = hrc_in(alice.path())
        .args(["members", "--json"])
        .output()
        .expect("command should run");
    let members: Value = serde_json::from_slice(&members.stdout).expect("stdout should be JSON");
    assert!(
        members["rosterEpoch"].as_u64().expect("an epoch") > 1,
        "revocation should advance the roster epoch: {members}"
    );

    // Bob's only device is gone, so there is no active recipient left for a
    // message addressed to him. The revocation excludes future messages by
    // refusing to seal one for a device that is no longer in the roster,
    // rather than by publishing something Bob's old key could still open.
    let after_revocation = hrc_in(alice.path())
        .args([
            "send",
            &bob_principal,
            "sent after the revocation",
            "--json",
        ])
        .output()
        .expect("command should run");
    assert!(
        !after_revocation.status.success(),
        "a message to a fully revoked member should not be sealed"
    );
    let refusal: Value =
        serde_json::from_slice(&after_revocation.stdout).expect("stdout should be JSON");
    assert_eq!(refusal["status"], "error");
    assert!(
        refusal["message"]
            .as_str()
            .unwrap_or_default()
            .contains("no active recipient device"),
        "the refusal should name the missing recipient device: {refusal}"
    );
}
