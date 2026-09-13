//! Naming the two local endpoints on each platform.
//!
//! The daemon's authority boundary is the endpoint: a connection's rights
//! come from which listener accepted it, never from anything the caller sent
//! (PRD section 19.5). So the two names must be distinct, predictable enough
//! for a client to find without configuration, and — on Unix — placed
//! somewhere only the owning user can reach.
//!
//! On Unix that is a socket file in a directory created with mode `0700`.
//! Permissions on the socket file itself are not portable enough to rely on
//! alone: several systems ignore them on connect. The directory is the part
//! every Unix enforces, so the directory is what this uses, and the socket
//! mode is set as well rather than instead.
//!
//! On Windows it is a named pipe. Pipe names are per-session, and the
//! default security descriptor already restricts a `\\.\pipe\` object to the
//! creating user's session.

use std::path::{Path, PathBuf};

#[cfg(not(windows))]
use crate::IpcError;
use crate::Result;

/// Which of the daemon's two interfaces an endpoint addresses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Interface {
    /// Reachable by agents and by `--json` callers.
    AgentSafe,
    /// Reachable only by the trusted local human interface.
    TrustedHuman,
}

impl Interface {
    /// The suffix that distinguishes this interface's endpoint.
    pub fn name(self) -> &'static str {
        match self {
            Interface::AgentSafe => "agent",
            Interface::TrustedHuman => "trusted",
        }
    }
}

/// A resolved local endpoint name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoint {
    interface: Interface,
    name: String,
}

impl Endpoint {
    /// Builds the endpoint for one interface under a runtime directory.
    ///
    /// On Windows the directory is ignored, because a named pipe is not a
    /// filesystem object; taking it anyway keeps one signature for both
    /// platforms so callers do not grow `cfg` branches of their own.
    pub fn new(runtime_dir: &Path, interface: Interface) -> Result<Self> {
        #[cfg(windows)]
        let name = {
            // A namespaced name, not a path: `interprocess` prefixes
            // `\\.\pipe\` itself, and passing an already-prefixed name
            // would produce a pipe nobody can find.
            //
            // Pipe names are machine-global, so the runtime directory is
            // folded in. That keeps the name stable for one installation
            // while stopping two users, two installations, or two tests from
            // colliding — and a collision here would mean one daemon
            // answering for another's endpoint.
            format!("hrc-{}-{}", directory_tag(runtime_dir), interface.name())
        };

        #[cfg(not(windows))]
        let name = {
            let path = runtime_dir.join(format!("hrc-{}.sock", interface.name()));
            path.to_str()
                .ok_or_else(|| {
                    IpcError::InvalidEndpoint(format!("{} is not valid UTF-8", path.display()))
                })?
                .to_owned()
        };

        Ok(Self { interface, name })
    }

    /// The platform-specific name a listener binds and a client connects to.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Which interface this endpoint addresses.
    pub fn interface(&self) -> Interface {
        self.interface
    }

    /// The socket file, on platforms where the endpoint is one.
    pub fn path(&self) -> Option<PathBuf> {
        (!cfg!(windows)).then(|| PathBuf::from(&self.name))
    }
}

#[cfg(windows)]
/// A short, stable tag derived from the runtime directory.
///
/// Hashed rather than embedded, because a pipe name has a length limit and a
/// path does not, and because a path contains characters a pipe name may not.
fn directory_tag(runtime_dir: &Path) -> String {
    use sha2::{Digest as _, Sha256};

    let mut hasher = Sha256::new();
    hasher.update(runtime_dir.to_string_lossy().as_bytes());
    hex_prefix(&hasher.finalize())
}

#[cfg(windows)]
/// The first four bytes of a digest, as hex.
fn hex_prefix(digest: &[u8]) -> String {
    digest
        .iter()
        .take(4)
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Creates the runtime directory with owner-only permissions.
///
/// On Unix the directory is the enforcement point, so this refuses to
/// continue if it cannot make it `0700`. Returning an unprotected directory
/// would hand back something that looks like a private location and is not.
pub fn prepare_runtime_dir(runtime_dir: &Path) -> Result<()> {
    std::fs::create_dir_all(runtime_dir)?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;

        let permissions = std::fs::Permissions::from_mode(0o700);
        std::fs::set_permissions(runtime_dir, permissions)?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_two_interfaces_get_distinct_names() {
        // If these ever collided, the trusted interface would be reachable
        // by whoever connected first.
        let dir = tempfile::tempdir().unwrap();

        let agent = Endpoint::new(dir.path(), Interface::AgentSafe).unwrap();
        let trusted = Endpoint::new(dir.path(), Interface::TrustedHuman).unwrap();

        assert_ne!(agent.name(), trusted.name());
        assert!(agent.name().contains("agent"));
        assert!(trusted.name().contains("trusted"));
    }

    #[test]
    fn an_endpoint_reports_the_interface_it_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(dir.path(), Interface::TrustedHuman).unwrap();

        assert_eq!(endpoint.interface(), Interface::TrustedHuman);
    }

    #[cfg(unix)]
    #[test]
    fn the_runtime_directory_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::tempdir().unwrap();
        let runtime = parent.path().join("run");
        prepare_runtime_dir(&runtime).unwrap();

        let mode = std::fs::metadata(&runtime).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o700,
            "the runtime directory must not be reachable by group or others"
        );
    }

    #[cfg(unix)]
    #[test]
    fn an_existing_permissive_directory_is_tightened() {
        // An upgrade, or a directory someone created by hand, must not leave
        // the sockets in a world-reachable place.
        use std::os::unix::fs::PermissionsExt as _;

        let parent = tempfile::tempdir().unwrap();
        let runtime = parent.path().join("run");
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::set_permissions(&runtime, std::fs::Permissions::from_mode(0o777)).unwrap();

        prepare_runtime_dir(&runtime).unwrap();

        let mode = std::fs::metadata(&runtime).unwrap().permissions().mode();
        assert_eq!(mode & 0o077, 0, "group and other access survived");
    }

    #[cfg(windows)]
    #[test]
    fn two_runtime_directories_get_distinct_pipe_names() {
        // Pipe names are machine-global. Two installations sharing one would
        // mean one daemon answering for the other's endpoint.
        let first = tempfile::tempdir().unwrap();
        let second = tempfile::tempdir().unwrap();

        assert_ne!(
            Endpoint::new(first.path(), Interface::AgentSafe)
                .unwrap()
                .name(),
            Endpoint::new(second.path(), Interface::AgentSafe)
                .unwrap()
                .name()
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_pipe_name_is_stable_for_one_runtime_directory() {
        let dir = tempfile::tempdir().unwrap();

        assert_eq!(
            Endpoint::new(dir.path(), Interface::AgentSafe).unwrap(),
            Endpoint::new(dir.path(), Interface::AgentSafe).unwrap()
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_endpoints_are_pipe_names_not_paths() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = Endpoint::new(dir.path(), Interface::AgentSafe).unwrap();

        assert!(
            !endpoint.name().contains('\\'),
            "a namespaced name, not a path"
        );
        assert!(endpoint.name().starts_with("hrc-"));
        assert!(endpoint.path().is_none());
    }
}
