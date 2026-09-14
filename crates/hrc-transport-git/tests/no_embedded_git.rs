//! The Git transport must invoke the system Git executable rather than
//! embed a Git implementation (PRD requirement HRC-TECH-010, decision
//! DEC-015).
//!
//! Stated as a dependency-graph property rather than a code-review habit.
//! The reason for the requirement is that a user's credential helpers,
//! `insteadOf` rules, proxy settings, signing configuration, and GitHub
//! authentication all live in their Git installation; an embedded library
//! reimplements some of that and silently ignores the rest. Someone adding
//! `git2` for one convenient call would not obviously be breaking anything —
//! which is exactly why a test says so.

use std::path::Path;

/// Crates that embed Git rather than calling it.
const EMBEDDED_GIT: [&str; 4] = ["git2", "libgit2-sys", "gix", "gitoxide-core"];

#[test]
fn no_embedded_git_implementation_is_in_the_dependency_graph() {
    let lockfile = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../Cargo.lock")
        .canonicalize()
        .expect("the workspace lockfile should resolve");

    let locked = std::fs::read_to_string(&lockfile).expect("read the lockfile");

    for crate_name in EMBEDDED_GIT {
        let entry = format!("name = \"{crate_name}\"");
        assert!(
            !locked.contains(&entry),
            "`{crate_name}` is in the dependency graph. PRD requirement HRC-TECH-010 requires \
             invoking the system Git executable so the user's credential helpers and \
             authentication stay authoritative; an embedded implementation bypasses them."
        );
    }
}

#[test]
fn the_transport_runs_git_as_a_program() {
    // The positive half. The negative test above would also pass if this
    // crate had stopped using Git altogether.
    let source = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/git.rs"))
        .expect("the git module should exist");

    assert!(
        source.contains("Command::new(\"git\")"),
        "the transport should invoke the system git executable"
    );
}

#[test]
fn the_canonicalization_implementation_is_pinned_exactly() {
    // PRD requirement HRC-TECH-005. Canonicalization is consensus-critical:
    // two peers that serialize the same payload differently compute
    // different digests and reject each other's signatures. A compatible
    // range would let a patch release change an escaping decision and break
    // every channel silently.
    let manifest =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Cargo.toml"))
            .expect("the workspace manifest should exist");

    let requirement = manifest
        .lines()
        .find_map(|line| line.trim().strip_prefix("serde_jcs = "))
        .expect("serde_jcs should be a workspace dependency");

    assert!(
        requirement.contains("=0."),
        "serde_jcs is `{requirement}`; RFC 8785 must be pinned exactly, not to a range"
    );
}
