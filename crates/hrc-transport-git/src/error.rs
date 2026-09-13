//! Errors from the Git transport.

/// Something went wrong invoking or interpreting Git.
#[derive(Debug, thiserror::Error)]
pub enum GitError {
    /// A git command exited non-zero.
    #[error("git command failed{}: {stderr}", match status {
        Some(code) => format!(" with status {code}"),
        None => String::new(),
    })]
    Command {
        /// Exit status, when one was reported.
        status: Option<i32>,
        /// Standard error, trimmed.
        stderr: String,
    },

    /// An operating-system call failed.
    #[error("could not {action}: {source}")]
    Io {
        /// What was being attempted.
        action: &'static str,
        /// The underlying failure.
        #[source]
        source: std::io::Error,
    },
}

/// Convenience alias for Git results.
pub type Result<T> = std::result::Result<T, GitError>;
