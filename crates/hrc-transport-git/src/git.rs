//! Thin wrapper over the system `git` executable.
//!
//! PRD requirement HRC-TECH-010: invoke the installed Git rather than
//! embedding a Git implementation, so the user's existing credential
//! helpers and host authentication stay authoritative.
//!
//! Every command runs with an explicit `GIT_DIR`, an isolated index file,
//! and an environment scrubbed of the variables that would otherwise let a
//! user's configuration change what a command means.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use crate::error::{GitError, Result};

/// A repository this adapter manages.
///
/// This is HRC's own bare repository, not a user checkout. Nothing here ever
/// touches a working tree, so a channel cannot disturb whatever the user has
/// open.
#[derive(Debug, Clone)]
pub struct GitRepository {
    git_dir: PathBuf,
}

impl GitRepository {
    /// Wraps an existing repository directory.
    pub fn new(git_dir: impl Into<PathBuf>) -> Self {
        Self {
            git_dir: git_dir.into(),
        }
    }

    /// The directory holding the repository.
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Creates the bare repository if it does not exist yet.
    pub fn init_bare(&self) -> Result<()> {
        if self.git_dir.join("HEAD").exists() {
            return Ok(());
        }

        std::fs::create_dir_all(&self.git_dir).map_err(|source| GitError::Io {
            action: "create the repository directory",
            source,
        })?;

        self.run_raw(
            None,
            [
                OsStr::new("init"),
                OsStr::new("--bare"),
                OsStr::new("--quiet"),
                self.git_dir.as_os_str(),
            ],
        )?;
        Ok(())
    }

    /// Points `origin` at `url`, replacing any existing definition.
    pub fn set_remote(&self, url: &str) -> Result<()> {
        // `remote set-url` fails when the remote does not exist and `remote
        // add` fails when it does, so set the config key directly.
        self.run(["config", "remote.origin.url", url])?;
        self.run([
            "config",
            "remote.origin.fetch",
            "+refs/heads/*:refs/remotes/origin/*",
        ])?;
        Ok(())
    }

    /// Runs a git command, returning its standard output.
    pub fn run<I, S>(&self, arguments: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run_raw(None, arguments)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs a git command and returns its raw standard output.
    pub fn run_bytes<I, S>(&self, arguments: I) -> Result<Vec<u8>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        Ok(self.run_raw(None, arguments)?.stdout)
    }

    /// Runs a git command with `input` on standard input.
    pub fn run_with_input<I, S>(&self, arguments: I, input: &[u8]) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run_raw(Some(input), arguments)?;
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    }

    /// Runs a git command, allowing a non-zero exit.
    ///
    /// Used where failure is an expected outcome rather than an error, such
    /// as a push rejected because the branch moved.
    pub fn try_run<I, S>(&self, arguments: I) -> Result<std::result::Result<String, String>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.spawn(None, arguments)?;
        if output.status.success() {
            Ok(Ok(String::from_utf8_lossy(&output.stdout).into_owned()))
        } else {
            Ok(Err(String::from_utf8_lossy(&output.stderr).into_owned()))
        }
    }

    /// Runs a command and fails on a non-zero exit.
    fn run_raw<I, S>(&self, input: Option<&[u8]>, arguments: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.spawn(input, arguments)?;

        if output.status.success() {
            Ok(output)
        } else {
            Err(GitError::Command {
                status: output.status.code(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
            })
        }
    }

    /// Spawns git and collects its output.
    fn spawn<I, S>(&self, input: Option<&[u8]>, arguments: I) -> Result<Output>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new("git");
        command
            .env("GIT_DIR", &self.git_dir)
            // An index in the repository directory would be shared between
            // concurrent commands; each caller supplies its own when it
            // needs one.
            .env("GIT_INDEX_FILE", self.git_dir.join("hrc-index"))
            // Deterministic, attributable commits that do not depend on the
            // user's global configuration.
            .env("GIT_AUTHOR_NAME", "hrc")
            .env("GIT_AUTHOR_EMAIL", "hrc@localhost")
            .env("GIT_COMMITTER_NAME", "hrc")
            .env("GIT_COMMITTER_EMAIL", "hrc@localhost")
            // Never block on a credential or passphrase prompt: a daemon has
            // no terminal, and a hung git process is worse than a failure.
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_OPTIONAL_LOCKS", "0")
            // A pager would never terminate when output is captured.
            .env("GIT_PAGER", "cat")
            .env("LC_ALL", "C")
            .args(arguments)
            .stdin(if input.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut child = command.spawn().map_err(|source| GitError::Io {
            action: "run the git executable",
            source,
        })?;

        if let Some(bytes) = input {
            use std::io::Write as _;
            let mut stdin = child.stdin.take().expect("stdin was piped");
            stdin.write_all(bytes).map_err(|source| GitError::Io {
                action: "write to git standard input",
                source,
            })?;
            drop(stdin);
        }

        child.wait_with_output().map_err(|source| GitError::Io {
            action: "collect git output",
            source,
        })
    }
}
