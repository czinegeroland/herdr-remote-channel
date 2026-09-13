//! The clean-host install fixture (PRD requirement HRC-TECH-012, acceptance
//! criterion `AC-RELEASE-ARTIFACTS`).
//!
//! The requirement is a negative one: installing must need no Rust toolchain
//! and no .NET, Node.js, or Python runtime on the end-user machine. A
//! negative like that cannot be proved by installing on a machine that has
//! all of them, so this fixture takes the real `scripts/install.sh`, feeds it
//! a real archive and a real checksum file, and runs it with `cargo`,
//! `rustc`, `rustup`, `node`, `python3`, and `dotnet` scrubbed from `PATH`.
//! If the script needed any of them, it would fail here.
//!
//! The archive is packed from the binary this test suite already built.
//! Standing in for a released artifact is exactly right for what is under
//! test: the packaging, verification, install, and smoke-test path, not the
//! compiler that produced the executable.
//!
//! Skipped on Windows, which installs through `install.ps1` rather than a
//! POSIX shell script.

#![cfg(unix)]

use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = "v0.0.0-fixture";

/// The repository root, found from this crate's manifest.
fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("the repository root should resolve")
}

/// The target triple this test is running on, in the form the installer
/// derives from `uname`.
fn host_target() -> &'static str {
    if cfg!(target_os = "linux") && cfg!(target_arch = "x86_64") {
        "x86_64-unknown-linux-gnu"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "x86_64") {
        "x86_64-apple-darwin"
    } else if cfg!(target_os = "macos") && cfg!(target_arch = "aarch64") {
        "aarch64-apple-darwin"
    } else {
        "unsupported"
    }
}

/// Builds a release-shaped archive and its checksum file in `into`.
///
/// The layout matches what `.github/workflows/release.yml` packages, because
/// the installer's job is to unpack that exact layout.
fn publish_fixture_release(into: &Path, target: &str) -> PathBuf {
    let staging_name = format!("hrc-{VERSION}-{target}");
    let staging = into.join(&staging_name);
    std::fs::create_dir_all(&staging).expect("staging directory");

    std::fs::copy(env!("CARGO_BIN_EXE_hrc"), staging.join("hrc")).expect("copy the executable");

    let archive = format!("{staging_name}.tar.gz");
    let status = Command::new("tar")
        .current_dir(into)
        .args(["-czf", &archive, &staging_name])
        .status()
        .expect("tar should be installed");
    assert!(status.success(), "could not pack the fixture archive");

    let digest = sha256_of(&into.join(&archive));
    std::fs::write(
        into.join(format!("{archive}.sha256")),
        format!("{digest}  {archive}\n"),
    )
    .expect("write the checksum file");

    into.join(archive)
}

fn sha256_of(path: &Path) -> String {
    let bytes = std::fs::read(path).expect("read the archive");
    hrc_protocol::canonical::sha256_hex(&bytes)
}

/// Runs the installer with every language runtime removed from `PATH`.
fn run_installer(release_dir: &Path, prefix: &Path, target: &str) -> std::process::Output {
    let root = repository_root();

    // A PATH with the ordinary system directories and nothing else. Anything
    // a developer machine adds — cargo's bin directory above all — is gone,
    // so the script cannot reach a toolchain even by accident.
    let clean_path = "/usr/bin:/bin:/usr/sbin:/sbin";

    let output = Command::new("sh")
        .arg(root.join("scripts/install.sh"))
        .args(["--version", VERSION])
        .arg("--prefix")
        .arg(prefix)
        .arg("--base-url")
        .arg(release_dir)
        .env_clear()
        .env("PATH", clean_path)
        .env("HOME", prefix)
        .output()
        .expect("the installer should run");

    let _ = target;
    output
}

#[test]
fn a_published_artifact_installs_and_runs_with_no_toolchain_present() {
    let target = host_target();
    if target == "unsupported" {
        // The installer refuses unknown platforms by design; there is no
        // artifact to install here.
        return;
    }

    let release = tempfile::tempdir().expect("release directory");
    let prefix = tempfile::tempdir().expect("install prefix");
    publish_fixture_release(release.path(), target);

    let output = run_installer(release.path(), prefix.path(), target);
    assert!(
        output.status.success(),
        "install failed\nstdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Checksum verified"), "{stdout}");

    // The installed executable runs, with no Rust toolchain and no .NET,
    // Node.js, or Python runtime reachable.
    let installed = prefix.path().join("hrc");
    assert!(installed.is_file(), "the executable should be installed");

    for arguments in [vec!["--version"], vec!["--help"]] {
        let smoke = Command::new(&installed)
            .args(&arguments)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", prefix.path())
            .output()
            .expect("the installed executable should run");

        assert!(
            smoke.status.success(),
            "`hrc {}` failed on a clean host: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&smoke.stderr)
        );
    }
}

#[test]
fn a_tampered_artifact_is_refused_rather_than_installed() {
    // The whole reason a release publishes checksums. If this passed, the
    // verification would be decoration.
    let target = host_target();
    if target == "unsupported" {
        return;
    }

    let release = tempfile::tempdir().expect("release directory");
    let prefix = tempfile::tempdir().expect("install prefix");
    let archive = publish_fixture_release(release.path(), target);

    // Someone replaces the artifact after the checksum was published.
    let mut tampered = std::fs::read(&archive).expect("read the archive");
    tampered.extend_from_slice(b"an extra byte nobody signed for");
    std::fs::write(&archive, tampered).expect("rewrite the archive");

    let output = run_installer(release.path(), prefix.path(), target);

    assert!(
        !output.status.success(),
        "a tampered artifact must not install"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("checksum mismatch"), "{stderr}");
    assert!(
        !prefix.path().join("hrc").exists(),
        "nothing should be installed after a failed verification"
    );
}

#[test]
fn a_missing_checksum_file_stops_the_install() {
    // An artifact whose checksum cannot be fetched is not an artifact that
    // may be installed unverified.
    let target = host_target();
    if target == "unsupported" {
        return;
    }

    let release = tempfile::tempdir().expect("release directory");
    let prefix = tempfile::tempdir().expect("install prefix");
    let archive = publish_fixture_release(release.path(), target);
    std::fs::remove_file(format!("{}.sha256", archive.display())).expect("remove the checksum");

    let output = run_installer(release.path(), prefix.path(), target);

    assert!(!output.status.success(), "install should stop");
    assert!(!prefix.path().join("hrc").exists());
}

#[test]
fn the_installer_requires_a_version_rather_than_guessing_one() {
    let root = repository_root();
    let output = Command::new("sh")
        .arg(root.join("scripts/install.sh"))
        .output()
        .expect("the installer should run");

    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("--version is required"),
        "it should say what is missing"
    );
}

#[test]
fn the_release_workflow_covers_every_target_the_prd_requires() {
    // PRD section 13.2 lists four targets. A release that silently omitted
    // one would leave those users with no way to install, so the workflow
    // both builds all four and refuses to publish without them.
    let workflow = std::fs::read_to_string(repository_root().join(".github/workflows/release.yml"))
        .expect("the release workflow should exist");

    for target in [
        "x86_64-pc-windows-msvc",
        "x86_64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        assert!(
            workflow.matches(target).count() >= 2,
            "`{target}` should be built and then checked for before publishing"
        );
    }
}

#[test]
fn the_release_toolchain_is_pinned_rather_than_tracked() {
    // Open question OQ-012, answered by decision DEC-050: a released binary
    // must be rebuildable from its tag, which `stable` does not give.
    let workflow = std::fs::read_to_string(repository_root().join(".github/workflows/release.yml"))
        .expect("the release workflow should exist");

    let pinned = workflow
        .lines()
        .find_map(|line| line.trim().strip_prefix("RUSTUP_TOOLCHAIN:"))
        .map(str::trim)
        .expect("the release workflow should pin a toolchain");

    let mut parts = pinned.split('.');
    assert!(
        parts.clone().count() == 3 && parts.all(|part| part.chars().all(|c| c.is_ascii_digit())),
        "`{pinned}` should be an exact version, not a channel"
    );

    // Development stays on the channel; only releases are pinned.
    let toolchain = std::fs::read_to_string(repository_root().join("rust-toolchain.toml"))
        .expect("rust-toolchain.toml should exist");
    assert!(
        toolchain.contains("channel = \"stable\""),
        "everyday development should stay on stable"
    );
}

#[test]
fn both_installers_verify_a_checksum_before_installing() {
    // The Windows installer cannot be exercised from this fixture, which is
    // POSIX-only, so its contract is checked by reading it. The property
    // that matters is the same on both: an artifact whose digest does not
    // match the published one is refused, not installed and reported.
    let root = repository_root();

    for script in ["scripts/install.sh", "scripts/install.ps1"] {
        let text = std::fs::read_to_string(root.join(script))
            .unwrap_or_else(|_| panic!("{script} should exist"));
        let lower = text.to_lowercase();

        assert!(
            lower.contains("checksum mismatch"),
            "{script} should refuse a mismatched artifact"
        );
        assert!(
            lower.contains("refusing to install"),
            "{script} should say it refused rather than warn and continue"
        );
        assert!(
            lower.contains("sha256") || lower.contains("sha-256"),
            "{script} should verify a SHA-256 digest"
        );
    }
}

#[test]
fn no_installer_asks_the_user_for_a_toolchain() {
    // HRC-TECH-012 is a negative requirement, and the way it is usually
    // broken is an installer that helpfully falls back to `cargo install`.
    let root = repository_root();

    for script in ["scripts/install.sh", "scripts/install.ps1"] {
        let text = std::fs::read_to_string(root.join(script))
            .unwrap_or_else(|_| panic!("{script} should exist"));

        for forbidden in [
            "cargo install",
            "cargo build",
            "rustup toolchain",
            "npm install",
        ] {
            assert!(
                !text.contains(forbidden),
                "{script} must not fall back to `{forbidden}`; installing may not need a toolchain"
            );
        }
    }
}
