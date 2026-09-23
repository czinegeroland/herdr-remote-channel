//! Checkable references on a delegation result (docs/RESEARCH.md 5.3).
//!
//! A result says what an assignee did. Most of the time that is only their
//! word for it, and "the fix is on main" is the kind of claim that turns out
//! not to be true often enough to measure. A reference makes one part of the
//! claim checkable: the result names a commit, and the requester's own
//! checkout says whether that commit exists.
//!
//! Both halves here are local and read-only. The sender resolves what they
//! typed to a full object name in their checkout; the receiver asks their
//! checkout whether it holds that object. Nothing fetches, checks out, merges
//! or applies anything, and nothing a sender wrote reaches `git` except a
//! validated hexadecimal object name.

use super::*;

/// Resolves a revision to the full object name of a commit in the checkout
/// at `root`.
///
/// Accepts anything `git rev-parse` does — `HEAD`, a branch, an abbreviation
/// — because that is what a person has to hand. What travels is the full
/// name, because an abbreviation can resolve to a different object in a
/// different repository.
pub(super) fn resolve_commit(root: &Path, revision: &str) -> Result<String> {
    let unresolved = || CliError::UnresolvedCommit {
        revision: revision.to_owned(),
    };

    // An option-shaped revision is refused outright rather than trusted to
    // `--end-of-options`, which older Git does not understand.
    if revision.is_empty() || revision.starts_with('-') {
        return Err(unresolved());
    }

    let output = ProcessCommand::new("git")
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", "--quiet"])
        .arg(format!("{revision}^{{commit}}"))
        .output()
        .map_err(|source| CliError::Io {
            action: "resolve a commit named in a result",
            source,
        })?;

    if !output.status.success() {
        return Err(unresolved());
    }

    let name = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let reference = hrc_protocol::Reference::commit(&name);
    reference.validate().map_err(|_| unresolved())?;

    Ok(name)
}

/// What this checkout knows about a commit a result names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CommitCheck {
    /// The commit exists here and the checked-out `HEAD` contains it.
    InHistory,
    /// The commit exists here but is not in `HEAD`'s history.
    Present,
    /// This checkout has no such commit.
    Missing,
    /// The review is not running inside a Git checkout, so nothing could be
    /// asked.
    NoCheckout,
}

/// Asks the checkout at `root` about one commit.
///
/// `id` must already be a validated full object name: it is passed to Git,
/// and a sender chose it.
pub(crate) fn check_commit(root: &Path, id: &str) -> CommitCheck {
    let git = |arguments: &[&str]| {
        ProcessCommand::new("git")
            .arg("-C")
            .arg(root)
            .args(arguments)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .ok()
    };

    let inside = git(&["rev-parse", "--is-inside-work-tree"]);
    if !inside.is_some_and(|status| status.success()) {
        return CommitCheck::NoCheckout;
    }

    let object = format!("{id}^{{commit}}");
    if !git(&["cat-file", "-e", &object]).is_some_and(|status| status.success()) {
        return CommitCheck::Missing;
    }

    match git(&["merge-base", "--is-ancestor", id, "HEAD"]) {
        Some(status) if status.success() => CommitCheck::InHistory,
        _ => CommitCheck::Present,
    }
}

/// The lines the trusted review shows beside a result's body.
///
/// They are this machine's findings about the result, not part of it, and
/// the screen draws them in a frame of their own for that reason. A message
/// of any other kind gets none.
///
/// The checkout asked is the one the review is running in, which is where a
/// person would look for the commit themselves.
pub(crate) fn local_checks(kind: &str, body: &str) -> Vec<String> {
    match std::env::current_dir() {
        Ok(root) => local_checks_with(kind, body, |id| check_commit(&root, id)),
        Err(_) => local_checks_with(kind, body, |_| CommitCheck::NoCheckout),
    }
}

/// [`local_checks`] with the checkout question supplied, so the wording can
/// be tested without a repository.
pub(crate) fn local_checks_with(
    kind: &str,
    body: &str,
    check: impl Fn(&str) -> CommitCheck,
) -> Vec<String> {
    if kind != hrc_protocol::MessageKind::Result.as_str() {
        return Vec::new();
    }

    let Ok(result) = serde_json::from_str::<hrc_protocol::ResultBody>(body) else {
        return vec!["This result could not be read, so nothing in it was checked.".into()];
    };

    if result.references.is_empty() {
        return vec![
            "This result names no commit. What it says was done is the sender's word.".into(),
        ];
    }

    result
        .references
        .iter()
        .take(hrc_protocol::delegation::MAX_REFERENCES)
        .map(|reference| {
            if reference.validate().is_err() {
                return "A reference in this result is malformed and was not checked.".to_owned();
            }

            if reference.kind != hrc_protocol::delegation::REFERENCE_COMMIT {
                return "A reference of a kind this version does not know was not checked."
                    .to_owned();
            }

            let short = &reference.id[..12];
            match check(&reference.id) {
                CommitCheck::InHistory => {
                    format!("commit {short}: found here, and your HEAD contains it")
                }
                CommitCheck::Present => {
                    format!("commit {short}: found here, but not in your HEAD's history")
                }
                CommitCheck::Missing => format!(
                    "commit {short}: CLAIMED BUT NOT FOUND in this checkout (fetching may bring it)"
                ),
                CommitCheck::NoCheckout => {
                    format!("commit {short}: not checked, this is not running in a Git checkout")
                }
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
