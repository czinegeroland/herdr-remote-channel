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

#[test]
fn the_npm_channel_verifies_what_it_publishes() {
    // The reason this project builds its own npm packages rather than using
    // the installer `cargo dist` generates: that one fetches the release
    // archive over HTTPS at install time and checks nothing. This project
    // publishes SHA-256 checksums so an artifact can be refused, and a
    // distribution channel that ignores them discards the property.
    //
    // Checked by reading, because running it needs four platform archives.
    // `scripts/build-npm-packages.mjs` was exercised against real archives
    // and refuses a tampered one, a missing checksum file, and a missing
    // platform, each with a non-zero exit.
    let builder = read("scripts/build-npm-packages.mjs");

    assert!(
        builder.contains("createHash('sha256')"),
        "the builder should compute a SHA-256 of each archive"
    );
    assert!(
        builder.contains("checksum mismatch") && builder.contains("Refusing to install"),
        "a mismatch should refuse rather than warn and continue"
    );
    assert!(
        builder.contains("has no published checksum"),
        "an archive with no checksum beside it should be refused, not trusted"
    );

    // Bundled rather than fetched. A `postinstall` in the published package
    // would mean an unverified download on every install, which is the thing
    // being avoided.
    //
    // Asserted against what the builder *emits* rather than against the word
    // appearing anywhere: both files explain in prose why there is no
    // postinstall, and a substring search on the bare word matches the
    // explanation. That is a test failing on its own documentation.
    for emitted in ["postinstall:", "\"postinstall\""] {
        assert!(
            !builder.contains(emitted),
            "the published package.json must declare no `{emitted}` hook"
        );
    }

    let shim = read("npm/hrc/bin.js");
    for forbidden in ["https://", "fetch(", "http.get", "require('node:https')"] {
        assert!(
            !shim.contains(forbidden),
            "the published shim must not reach the network: found `{forbidden}`"
        );
    }
    assert!(
        builder.contains("optionalDependencies"),
        "platforms should be selected by npm through optional dependencies"
    );
}

#[test]
fn the_npm_shim_and_its_builder_agree_on_the_platforms() {
    // Two lists of the same four targets, in different languages, in
    // different files. They drift the moment a platform is added to one and
    // not the other, and the failure would be a person on that platform
    // installing a package with no binary in it.
    let shim = read("npm/hrc/bin.js");
    let builder = read("scripts/build-npm-packages.mjs");

    for suffix in ["linux-x64", "darwin-x64", "darwin-arm64", "win32-x64"] {
        assert!(
            shim.contains(suffix),
            "the shim does not resolve `{suffix}`"
        );
        assert!(
            builder.contains(suffix),
            "the builder does not build `{suffix}`"
        );
    }

    // And the same four the release pipeline is configured to produce.
    let workspace = read("Cargo.toml");
    for target in [
        "x86_64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        assert!(
            workspace.contains(target),
            "`{target}` is packaged for npm but not built by the release pipeline"
        );
        assert!(
            builder.contains(target),
            "`{target}` is built by the release pipeline but not packaged for npm"
        );
    }
}

/// The JavaScript in this repository with `//` comment lines dropped, so a
/// check about emitted values cannot be satisfied or broken by prose.
fn code_only(source: &str) -> String {
    source
        .lines()
        .filter(|line| !line.trim_start().starts_with("//"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_npm_packages_are_named_without_a_scope() {
    // The first real publish failed here. A scope on npm is not a free-form
    // namespace: it resolves to a user or an organization, and publishing
    // under one nobody owns fails with a bare `E404 Not Found` on the PUT,
    // which reads like a missing package rather than a permissions problem.
    //
    // Unscoped names need nothing to exist beforehand. If a scope is wanted
    // later it has to be one the publishing account actually owns.
    //
    // Both files explain this in comments, which mention the bad scope by
    // name, so the check reads the code with comment lines removed. Asserting
    // on the whole file would fail on its own documentation.
    let builder = read("scripts/build-npm-packages.mjs");
    let shim = read("npm/hrc/bin.js");

    for text in [&builder, &shim] {
        assert!(
            !code_only(text).contains("@herdr-remote-channel"),
            "`@herdr-remote-channel` is a scope nobody owns; publishing under \
             it fails with E404"
        );
    }

    assert!(
        builder.contains("`${ROOT_PACKAGE}-${platform.suffix}`"),
        "platform packages should be named from the root package"
    );
    assert!(
        shim.contains("`herdr-remote-channel-${suffix}`"),
        "the shim should resolve the same unscoped names the builder writes"
    );
}

#[test]
fn the_readme_does_not_send_anyone_to_npx_hrc() {
    // `hrc` is an existing, unrelated package on npm owned by someone else,
    // so `npx hrc` downloads and runs their code. The binary is still called
    // `hrc` once installed; it is only the `npx` shorthand that is unsafe,
    // and it is exactly what someone would guess.
    let readme = read("README.md");

    assert!(
        !readme.contains("npx hrc@") && !readme.contains("npx hrc "),
        "the README must not tell anyone to run `npx hrc`"
    );
    assert!(
        readme.contains("npx herdr-remote-channel"),
        "the README should give the full package name"
    );
    assert!(
        readme.contains("runs someone else's code"),
        "the README should say why the shorthand is unsafe, not just avoid it"
    );
}

#[test]
fn the_npm_publish_workflow_can_actually_be_triggered() {
    // `on: release: published` looks right and never fires. `cargo dist`
    // creates the release with `GITHUB_TOKEN`, and GitHub does not raise
    // workflow-triggering events for anything done with that token — the
    // recursion guard. The first release reached GitHub with nothing
    // published to npm because of exactly this.
    let workflow = read(".github/workflows/npm-publish.yml");

    assert!(
        !workflow.contains(
            "release:
    types:"
        ),
        "a `release:` trigger cannot fire for a release created by CI"
    );
    assert!(
        workflow.contains("workflow_run:"),
        "publishing should chain off the Release workflow completing"
    );
    assert!(
        workflow.contains("workflow_dispatch:"),
        "a failed publish should be retryable without cutting another tag"
    );
    assert!(
        workflow.contains("conclusion == 'success'"),
        "a failed Release run must not publish"
    );
}

#[test]
fn the_publish_step_never_hands_npm_a_bare_directory_path() {
    // `npm publish npm-dist/herdr-remote-channel` does not publish that
    // directory. npm reads a bare `a/b` argument as the GitHub shorthand
    // `owner/repo` and went looking for
    // `ssh://git@github.com/npm-dist/herdr-remote-channel.git`, which failed
    // on a public key after the four platform packages had already gone out.
    //
    // A leading `./` makes it unambiguously a path. The platform packages
    // only ever escaped this because the `npm-dist/*/` glob leaves a
    // trailing slash on each match.
    let workflow = read(".github/workflows/npm-publish.yml");

    for line in workflow
        .lines()
        .map(str::trim)
        .filter(|line| line.starts_with("npm publish"))
    {
        let argument = line
            .split_whitespace()
            .next_back()
            .expect("a publish command has arguments");

        assert!(
            argument.starts_with("\"./") || argument.starts_with("./"),
            "`{line}` gives npm a path it will read as `owner/repo`"
        );
    }
}

#[test]
fn a_partly_published_release_can_be_retried() {
    // npm creates each package separately and a published version is
    // immutable, so a run that fails partway leaves some already published.
    // The first real publish did exactly that: four platform packages
    // landed, the root package failed. Without a skip, retrying that run
    // aborts on the first package with `EPUBLISHCONFLICT` and the release
    // can never be completed — which would make the `workflow_dispatch`
    // retry this workflow offers useless precisely when it is needed.
    let workflow = read(".github/workflows/npm-publish.yml");

    assert!(
        workflow.contains("npm view \"$name@$version\" version"),
        "the publish step should check whether a version already exists"
    );
    assert!(
        workflow.contains("is already published"),
        "an existing version should be skipped rather than fail the run"
    );
}

#[test]
fn a_publish_refused_by_the_registry_is_retried_rather_than_reported() {
    // npm will not accept a new package name while it is still processing
    // the one before it, and answers with `E409 Failed to save packument`.
    // Publishing five new names in a row hits that every time: the first two
    // runs of v0.2.1 each landed exactly one package and were refused the
    // next, so the release could only ever be completed by dispatching the
    // workflow once per package.
    //
    // That is a property of the registry rather than of these packages, so
    // the step waits and asks again. It also re-checks the registry after a
    // failure, because `E409` reports a packument that failed to save and
    // only the registry knows whether it did; retrying one that actually
    // landed would fail as `EPUBLISHCONFLICT` and end the run.
    let workflow = read(".github/workflows/npm-publish.yml");

    assert!(
        workflow.contains("sleep \"$delay\""),
        "the publish step should wait before retrying a refused publish"
    );
    assert!(
        workflow.contains("delay=$((delay * 2))"),
        "successive retries should back off rather than hammer the registry"
    );
    assert!(
        workflow.contains("is published despite the error"),
        "a publish that landed despite an error should not be retried"
    );
}
