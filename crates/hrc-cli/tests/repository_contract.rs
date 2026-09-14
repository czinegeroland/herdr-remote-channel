//! What the repository must carry to be published.
//!
//! These are not tests of the program. They are tests of the claims the
//! repository makes about itself, which on a public repository are the first
//! thing anyone reads and the easiest thing to let drift. A `license` field
//! in `Cargo.toml` with no `LICENSE` file beside it is the standard example:
//! nothing fails to build, and the project has silently told two different
//! stories about what people may do with it.

use std::path::{Path, PathBuf};

fn repository_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("the crate lives two directories below the workspace root")
        .to_path_buf()
}

fn read(relative: &str) -> String {
    let path = repository_root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| {
        panic!("{} should be readable: {error}", path.display());
    })
}

#[test]
fn the_declared_license_has_a_license_file_that_matches_it() {
    // The workspace declares `license = "Apache-2.0"`. Publishing that with
    // no license text is worse than publishing nothing: a reader is told
    // there are terms and cannot find them.
    let workspace = read("Cargo.toml");
    assert!(
        workspace.contains("license = \"Apache-2.0\""),
        "the workspace should declare its license"
    );

    let license = read("LICENSE");
    assert!(
        license.contains("Apache License")
            && license.contains("Version 2.0, January 2004")
            && license.contains("http://www.apache.org/licenses/"),
        "LICENSE should be the Apache 2.0 text"
    );

    // The appendix boilerplate is the part people forget to fill in, and an
    // unfilled `[yyyy] [name of copyright owner]` is a licence that names
    // nobody.
    assert!(
        !license.contains("[yyyy]") && !license.contains("[name of copyright owner]"),
        "the LICENSE appendix still has its placeholder text"
    );
    assert!(
        license.contains("Copyright "),
        "LICENSE should name a copyright holder"
    );
}

#[test]
fn a_security_policy_points_reporters_somewhere_private() {
    // This project moves other people's private conversations. A reporter who
    // cannot find a private channel will use a public issue, and the first
    // people to read it will not be the maintainer.
    let policy = read("SECURITY.md");
    let lower = policy.to_lowercase();

    assert!(
        lower.contains("do not open a public issue"),
        "the policy should say plainly not to report publicly"
    );
    assert!(
        lower.contains("security/advisories/new") || lower.contains("private vulnerability"),
        "the policy should name a private reporting channel"
    );
    for expected in ["in scope", "out of scope"] {
        assert!(
            lower.contains(expected),
            "the policy should state what is `{expected}`, so a reporter is not guessing"
        );
    }
}

#[test]
fn contributing_states_the_two_rules_that_reject_a_pull_request() {
    // Both are unusual enough that a competent contributor would not guess
    // them, and both fail a pull request after the work is done.
    let contributing = read("CONTRIBUTING.md");
    let lower = contributing.to_lowercase();

    assert!(
        lower.contains("prd.md") && lower.contains("every pull request must update"),
        "contributors should learn the living-PRD rule before they spend effort"
    );
    assert!(
        lower.contains("22.7"),
        "contributors should learn the human authorization boundary"
    );
}

#[test]
fn every_third_party_action_is_pinned_to_an_immutable_revision() {
    // A tag is a mutable pointer. An action referenced by tag runs whatever
    // its owner most recently pushed there, inside this repository's CI.
    // GitHub's own `actions/*` are excluded deliberately: they are published
    // by the same party that runs the runner, so pinning them defends
    // against nothing that is not already game over.
    let root = repository_root();
    let workflows = std::fs::read_dir(root.join(".github/workflows"))
        .expect("the workflows directory should exist");

    let mut checked = 0;
    for entry in workflows {
        let path = entry.expect("a directory entry").path();
        if path.extension().is_none_or(|extension| extension != "yml") {
            continue;
        }

        let text = std::fs::read_to_string(&path).expect("a workflow should be readable");
        for line in text.lines() {
            let Some((_, reference)) = line.trim().split_once("uses: ") else {
                continue;
            };
            let reference = reference.split_whitespace().next().unwrap_or_default();
            let Some((action, version)) = reference.rsplit_once('@') else {
                continue;
            };
            if action.starts_with("actions/") || action.starts_with("./") {
                continue;
            }

            checked += 1;
            assert!(
                version.len() == 40 && version.chars().all(|c| c.is_ascii_hexdigit()),
                "{} pins `{action}` to `{version}`, which is a mutable tag rather than a \
                 commit; resolve it with `git ls-remote https://github.com/{action} \
                 refs/tags/{version}^{{}}`",
                path.display()
            );
        }
    }

    assert!(
        checked > 0,
        "this test found no third-party actions to check, which means it is \
         no longer testing anything"
    );
}

#[test]
fn the_dependency_exemption_stays_narrow() {
    // Enabling Dependabot made every dependency pull request permanently
    // unmergeable: the living-PRD rule asks for a requirement row to move,
    // and a version bump moves none. The exemption that fixes it is a hole
    // in the project's central enforcement mechanism, so its shape is worth
    // pinning.
    //
    // Two conditions must both survive. Dropping the author check would let
    // any contributor skip the rule by touching only manifests; dropping the
    // path check would let a bump carry a change to `crates/` past it. The
    // second was verified by constructing that exact commit and watching the
    // validator refuse it.
    let validator = read(".github/scripts/check-prd-traceability.ps1");

    assert!(
        validator.contains("dependabot[bot]"),
        "the exemption should name the one author it trusts"
    );
    assert!(
        validator.contains("'Bot'"),
        "the exemption should require the author be a bot account, not a \
         user who took the name"
    );
    assert!(
        validator.contains("$isDependabot -and $onlyManifests"),
        "both conditions should be required together; either alone is a hole"
    );

    // The check must pass for an exempt pull request rather than be skipped
    // by the workflow. A required status check that never runs blocks a
    // merge instead of satisfying it.
    let workflow = read(".github/workflows/prd-traceability.yml");
    assert!(
        !workflow.contains("dependabot"),
        "the exemption belongs in the validator, where it is one testable \
         rule, not in a workflow `if:` that skips the check entirely"
    );
}
