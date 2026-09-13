//! GitHub setup and change-detection optimization (PRD requirement
//! HRC-TR-006).
//!
//! PRD section 17.1 allows a GitHub adapter to use conditional API requests
//! and ETags instead of polling `git ls-remote`, and section 28 requires
//! that the Git transport not need a GitHub-specific API to work at all.
//! Those two pull in opposite directions, and the shape here is what keeps
//! them both true.
//!
//! # The optimization can only ever answer one question
//!
//! *Has the channel head moved?* That is all. It never returns a
//! publication, an object, or a byte of channel content — those still come
//! from plain Git, over the same code path a non-GitHub remote uses.
//!
//! This is not a stylistic choice, it is what makes the acceptance criterion
//! `AC-GIT-ADAPTER` ("GitHub optimization preserves identical protocol
//! behavior") true by construction rather than by testing. A faster way to
//! learn whether to fetch cannot change what a fetch returns, because it
//! does not participate in fetching. The worst a broken optimization can do
//! is make the daemon fetch when it need not have, or not fetch when it
//! could have — and the second is bounded, because an unusable optimization
//! reports [`Probe::Unavailable`] and the caller falls back to `ls-remote`.
//!
//! # Why it shells out to `gh`
//!
//! For the same reason the transport shells out to `git` (PRD section 13.2):
//! the user's existing GitHub authentication stays authoritative. HRC never
//! reads a token, never stores one, and never has one to leak. An
//! installation without `gh`, or with `gh` unauthenticated, simply falls
//! back — which is the behavior a non-GitHub remote gets anyway.

use std::process::Command;

/// A GitHub repository named by a remote locator.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHubRemote {
    /// The owning user or organization.
    pub owner: String,
    /// The repository name, without any `.git` suffix.
    pub repo: String,
}

impl GitHubRemote {
    /// Recognizes a GitHub remote, or returns `None` for anything else.
    ///
    /// Returning `None` is the common and correct case: a shared-folder
    /// path, a GitLab URL, or a bare local repository is not a failure, it
    /// is a channel that uses the generic path.
    ///
    /// Only `github.com` itself is matched. A GitHub Enterprise host uses a
    /// different API root, so treating one as `github.com` would send
    /// requests somewhere they do not belong; those installations get the
    /// generic path until someone implements the enterprise root properly.
    pub fn parse(locator: &str) -> Option<Self> {
        let locator = locator.trim();

        // `git@github.com:owner/repo.git`
        let rest = if let Some(rest) = locator.strip_prefix("git@github.com:") {
            rest
        } else {
            // `https://github.com/owner/repo`, `ssh://git@github.com/...`,
            // and the `http`/`git` schemes, with or without a trailing slash.
            let without_scheme = locator
                .split_once("://")
                .map(|(_, rest)| rest)
                .unwrap_or(locator);

            // Strip any `user@` or `user:password@` prefix before the host.
            let without_userinfo = without_scheme
                .split_once('@')
                .map(|(_, rest)| rest)
                .unwrap_or(without_scheme);

            without_userinfo.strip_prefix("github.com/")?
        };

        let rest = rest.trim_end_matches('/');
        let (owner, repo) = rest.split_once('/')?;
        let repo = repo.strip_suffix(".git").unwrap_or(repo);

        // A path with more segments is not a repository locator, and
        // guessing which two segments were meant is how a request ends up
        // addressed to the wrong repository.
        if owner.is_empty() || repo.is_empty() || repo.contains('/') {
            return None;
        }

        Some(Self {
            owner: owner.to_owned(),
            repo: repo.to_owned(),
        })
    }

    /// The `owner/repo` form `gh` takes.
    pub fn slug(&self) -> String {
        format!("{}/{}", self.owner, self.repo)
    }
}

/// What a conditional head request found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Probe {
    /// The provider confirmed the head has not moved.
    ///
    /// This is the whole point: GitHub answers a conditional request with
    /// `304 Not Modified`, which is cheap and does not count against the
    /// primary rate limit.
    Unchanged,
    /// The head is at this commit.
    Changed {
        /// The commit the channel branch points at.
        head: String,
        /// The validator to send with the next request, when one was given.
        etag: Option<String>,
    },
    /// The optimization could not be used. Fall back to generic Git.
    ///
    /// Carries a reason for diagnostics only. Every cause — `gh` absent,
    /// unauthenticated, rate limited, offline, a repository that does not
    /// exist — lands here, because the caller's response to all of them is
    /// the same and correct: use `ls-remote`.
    Unavailable(String),
}

/// Runs the `gh` command line.
///
/// A trait so the change detector can be tested without a network, a token,
/// or `gh` installed. The production implementation is [`GhCli`].
pub trait GhRunner {
    /// Runs `gh` with these arguments, returning its exit status, standard
    /// output, and standard error.
    fn run(&self, arguments: &[String]) -> std::io::Result<GhOutput>;
}

/// What one `gh` invocation produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GhOutput {
    /// Whether `gh` exited successfully.
    pub success: bool,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

/// Runs the real `gh` executable.
#[derive(Debug, Clone, Copy, Default)]
pub struct GhCli;

impl GhRunner for GhCli {
    fn run(&self, arguments: &[String]) -> std::io::Result<GhOutput> {
        let output = Command::new("gh").args(arguments).output()?;

        Ok(GhOutput {
            success: output.status.success(),
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }
}

/// Conditional head polling against the GitHub API.
///
/// Holds the last ETag so the next request can be conditional. Nothing else
/// is cached: the head this returns is used to decide whether to fetch, and
/// the fetch reads Git.
#[derive(Debug)]
pub struct HeadWatcher<R: GhRunner> {
    remote: GitHubRemote,
    branch: String,
    etag: Option<String>,
    runner: R,
}

impl<R: GhRunner> HeadWatcher<R> {
    /// Builds a watcher for a locator, or `None` if it is not a GitHub
    /// remote.
    pub fn for_locator(locator: &str, branch: &str, runner: R) -> Option<Self> {
        Some(Self {
            remote: GitHubRemote::parse(locator)?,
            branch: branch.to_owned(),
            etag: None,
            runner,
        })
    }

    /// The repository this watcher polls.
    pub fn remote(&self) -> &GitHubRemote {
        &self.remote
    }

    /// The runner this watcher calls, for diagnostics and tests.
    pub fn runner(&self) -> &R {
        &self.runner
    }

    /// The validator that will be sent with the next request, if any.
    pub fn etag(&self) -> Option<&str> {
        self.etag.as_deref()
    }

    /// Asks GitHub whether the channel branch has moved.
    pub fn poll(&mut self) -> Probe {
        let mut arguments = vec![
            "api".to_owned(),
            "--include".to_owned(),
            format!(
                "repos/{}/{}/commits/{}",
                self.remote.owner, self.remote.repo, self.branch
            ),
        ];

        // Only the commit SHA is wanted, so ask for the media type that
        // returns the least: a full commit carries the diff.
        arguments.push("-H".to_owned());
        arguments.push("Accept: application/vnd.github.sha".to_owned());

        if let Some(etag) = &self.etag {
            arguments.push("-H".to_owned());
            arguments.push(format!("If-None-Match: {etag}"));
        }

        let output = match self.runner.run(&arguments) {
            Ok(output) => output,
            // `gh` not installed is the ordinary case on most machines.
            Err(error) => return Probe::Unavailable(format!("could not run gh: {error}")),
        };

        let combined = format!("{}\n{}", output.stdout, output.stderr);

        // A conditional hit. `gh` exits non-zero on 304, so this is checked
        // before the success flag: treating it as a failure would throw away
        // the cheap answer the request was made for.
        if status_is(&combined, "304") {
            return Probe::Unchanged;
        }

        if !output.success {
            return Probe::Unavailable(
                first_line(&output.stderr)
                    .unwrap_or_else(|| "gh reported a failure with no message".to_owned()),
            );
        }

        let Some(head) = commit_sha(&output.stdout) else {
            return Probe::Unavailable("gh returned no commit SHA".to_owned());
        };

        let etag = header_value(&combined, "etag");
        self.etag = etag.clone();

        Probe::Changed { head, etag }
    }
}

/// Whether the response headers report this status code.
fn status_is(response: &str, code: &str) -> bool {
    response.lines().any(|line| {
        let line = line.trim();
        line.starts_with("HTTP/") && line.split_whitespace().nth(1) == Some(code)
    })
}

/// Reads one header value, case-insensitively.
fn header_value(response: &str, name: &str) -> Option<String> {
    response.lines().find_map(|line| {
        let (key, value) = line.split_once(':')?;
        key.trim()
            .eq_ignore_ascii_case(name)
            .then(|| value.trim().to_owned())
    })
}

/// Finds the 40-character commit SHA in a response body.
///
/// The `sha` media type returns the bare SHA, but `--include` prints headers
/// first, so the body is whatever follows them.
fn commit_sha(response: &str) -> Option<String> {
    response
        .lines()
        .map(str::trim)
        .find(|line| line.len() == 40 && line.chars().all(|c| c.is_ascii_hexdigit()))
        .map(str::to_owned)
}

/// The first non-empty line, for a one-line diagnostic.
fn first_line(text: &str) -> Option<String> {
    text.lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_owned)
}

/// The GitHub-specific setup steps for a channel, for `hrc doctor` and the
/// skill to show.
///
/// Advice, not automation. Creating a repository and granting access are
/// changes to who can read a channel, and PRD section 22.7 keeps decisions
/// of that kind with the human.
pub fn setup_steps(remote: &GitHubRemote) -> Vec<String> {
    vec![
        format!(
            "Create {} as a **private** repository. A public one exposes every \
             ciphertext object and the whole membership history to anyone.",
            remote.slug()
        ),
        format!(
            "Confirm `gh auth status` succeeds, or that a Git credential helper \
             can push to {}. HRC never stores a token of its own.",
            remote.slug()
        ),
        "Protect the `hrc` branch against force pushes and deletion. The \
         channel's tamper detection will halt on a rewrite, but a protected \
         branch stops one happening."
            .to_owned(),
        format!(
            "Add each member as a collaborator on {} only after verifying \
             their safety phrase out of band.",
            remote.slug()
        ),
    ]
}

#[cfg(test)]
mod tests;
