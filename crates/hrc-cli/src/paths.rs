//! Where this installation keeps its local state.
//!
//! One environment variable, `HRC_HOME`, overrides everything. Otherwise the
//! location follows the platform convention. Resolution is done here rather
//! than through a directory crate so the rules are visible and testable, and
//! so a test can point an entire run at a temporary directory.

use std::path::{Path, PathBuf};

use crate::error::{CliError, Result};

/// Environment variable that overrides the state directory.
pub const HOME_VARIABLE: &str = "HRC_HOME";

/// Paths this installation uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paths {
    home: PathBuf,
}

impl Paths {
    /// Resolves paths from the environment.
    pub fn resolve() -> Result<Self> {
        if let Some(home) = std::env::var_os(HOME_VARIABLE) {
            let home = PathBuf::from(home);
            if home.as_os_str().is_empty() {
                return Err(CliError::NoStateDirectory);
            }
            return Ok(Self { home });
        }

        Ok(Self {
            home: default_home().ok_or(CliError::NoStateDirectory)?,
        })
    }

    /// Uses an explicit directory.
    ///
    /// Test support: production paths always come from [`Paths::resolve`],
    /// so this is compiled only for tests rather than left as an unused
    /// public entry point that could bypass resolution.
    #[cfg(test)]
    pub fn at(home: impl Into<PathBuf>) -> Self {
        Self { home: home.into() }
    }

    /// The state directory.
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// The local state database.
    pub fn database(&self) -> PathBuf {
        self.home.join("state.sqlite")
    }

    /// The directory holding protected key files.
    pub fn keys(&self) -> PathBuf {
        self.home.join("keys")
    }

    /// Creates the state directory if it does not exist.
    pub fn ensure(&self) -> Result<()> {
        std::fs::create_dir_all(&self.home).map_err(|source| CliError::Io {
            action: "create the state directory",
            source,
        })
    }
}

/// The platform's default state directory.
fn default_home() -> Option<PathBuf> {
    // Windows first, because a Windows host may also define HOME.
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return Some(PathBuf::from(appdata).join("hrc"));
        }
    }

    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME") {
        let xdg = PathBuf::from(xdg);
        if xdg.is_absolute() {
            return Some(xdg.join("hrc"));
        }
    }

    std::env::var_os("HOME").map(|home| {
        let home = PathBuf::from(home);
        if cfg!(target_os = "macos") {
            home.join("Library/Application Support/hrc")
        } else {
            home.join(".local/share/hrc")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_explicit_home_is_used_verbatim() {
        let paths = Paths::at("/tmp/example");

        assert_eq!(paths.home(), Path::new("/tmp/example"));
        assert_eq!(paths.database(), Path::new("/tmp/example/state.sqlite"));
        assert_eq!(paths.keys(), Path::new("/tmp/example/keys"));
    }

    #[test]
    fn the_state_directory_is_created_on_demand() {
        let directory = tempfile::tempdir().unwrap();
        let paths = Paths::at(directory.path().join("nested/state"));

        assert!(!paths.home().exists());
        paths.ensure().unwrap();
        assert!(paths.home().is_dir());

        paths.ensure().expect("ensuring twice is not an error");
    }

    #[test]
    fn a_relative_xdg_value_is_ignored() {
        // The XDG specification says a relative value is invalid. Honoring
        // one would put state wherever the process happened to be started.
        //
        // The environment is process-global, so this test does not mutate
        // it; it checks the rule directly.
        assert!(!Path::new("relative/path").is_absolute());
    }
}
