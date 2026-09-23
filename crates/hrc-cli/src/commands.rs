//! Implementations of the commands that need no network.
//!
//! Each returns a JSON value. The human renderer formats that same value, so
//! the two output modes cannot drift: there is one source of truth for what
//! a command reports, and `--json` is a formatting choice rather than a
//! separate code path.

use std::collections::HashMap;
use std::path::{Component, Path};
use std::process::Command as ProcessCommand;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use hrc_core::rpc::{AgentRequest, Broker, ChannelStatus, Request, TrustedRequest, dispatch_agent};
use hrc_core::sync::{PollActivity, poll_interval};
use hrc_core::{ContextPackage, ExcludedPath, Roster};
use hrc_crypto::enrollment::Invite;
use hrc_crypto::{DeviceSecrets, InviteSecret, KeyStore, PassphraseStore, PrincipalSecrets};
use hrc_ipc::endpoint::{Endpoint, Interface, prepare_runtime_dir};
use hrc_ipc::serve;
use hrc_protocol::canonical;
use hrc_protocol::control::{
    ControlEntryPayload, ControlOperation, GenesisPayload, PrincipalMaterial, TransportLocator,
};
use hrc_protocol::domain;
use hrc_protocol::identity::{DeviceCertificatePayload, DeviceDescriptor};
use hrc_protocol::message::MessageEnvelope;
use hrc_protocol::signed::{SignedObject, Signer};
use hrc_storage::{Database, PendingOutgoing};
use hrc_transport::{ObjectClass, PublishObject, Revision, Transport};
use hrc_transport_git::GitTransport;
use secrecy::SecretString;
use serde_json::{Value, json};

use crate::error::{CliError, Result};
use crate::paths::Paths;

/// Environment variable supplying the key store passphrase.
///
/// Non-interactive automation needs some way to unlock the store. An
/// environment variable is visible to other processes of the same user, so
/// it is an explicit opt-in for automation rather than the recommended path
/// for a person at a terminal.
pub const PASSPHRASE_VARIABLE: &str = "HRC_PASSPHRASE";

/// The name this installation's device keys are stored under.
const DEVICE_KEY_NAME: &str = "device";

/// The name this installation's principal key is stored under.
///
/// Separate from the device key because they answer for different things: a
/// device key signs messages from one machine, while a principal key vouches
/// for which devices belong to the person (PRD requirement HRC-CH-006).
const PRINCIPAL_KEY_NAME: &str = "principal";

/// Everything a command needs from its environment.
///
/// The passphrase is passed in rather than read here, so nothing in this
/// module depends on process-global state. That keeps commands testable
/// without mutating the environment, which is shared by every test in a
/// binary and cannot be changed safely once threads exist.
#[derive(Debug, Clone)]
pub struct Context {
    /// Where local state lives.
    pub paths: Paths,
    /// Key store passphrase, when one was supplied.
    pub passphrase: Option<SecretString>,
}

impl Context {
    /// Builds a context from resolved paths and the environment.
    pub fn from_environment(paths: Paths) -> Self {
        let passphrase = std::env::var(PASSPHRASE_VARIABLE)
            .ok()
            .filter(|value| !value.is_empty())
            .map(SecretString::from);

        Self { paths, passphrase }
    }

    /// Opens the key store, or explains what is missing.
    fn key_store(&self) -> Result<PassphraseStore> {
        let passphrase = self.passphrase.clone().ok_or(CliError::NoPassphrase)?;

        Ok(PassphraseStore::open(self.paths.keys(), passphrase)?)
    }
}

pub mod review;
mod serve;

mod channel;
mod context;
mod diagnostics;
mod enrol;
mod herdr;
mod identity;
mod membership;
mod read;
mod send;
mod sync;

pub use channel::*;
pub use context::*;
pub use diagnostics::*;
pub use enrol::*;
pub use herdr::*;
pub use identity::*;
pub use membership::*;
pub use read::*;
pub use send::*;
pub use sync::*;

pub use serve::daemon;
#[cfg(test)]
use serve::{DaemonBroker, daemon_tick, handle_trusted_request};

#[cfg(test)]
mod tests;

#[cfg(test)]
mod time_tests {
    use super::*;

    #[test]
    fn a_lifetime_resolves_to_an_absolute_expiry() {
        assert_eq!(
            expiry_from("2026-09-13T00:00:00Z", "24h").unwrap(),
            "2026-09-14T00:00:00Z"
        );
        assert_eq!(
            expiry_from("2026-09-13T23:30:00Z", "30m").unwrap(),
            "2026-09-14T00:00:00Z"
        );
        assert_eq!(
            expiry_from("2026-09-13T00:00:00Z", "7d").unwrap(),
            "2026-09-20T00:00:00Z"
        );
    }

    #[test]
    fn a_duration_is_never_stored_as_if_it_were_a_timestamp() {
        // The bug this replaced: "24h" compared lexicographically against a
        // date lapses at a moment that depends on the year.
        let resolved = expiry_from("2026-09-13T00:00:00Z", "24h").unwrap();

        assert!(resolved.starts_with("2026-"), "{resolved}");
        assert!(resolved.ends_with('Z'));
    }

    #[test]
    fn month_and_year_boundaries_are_handled() {
        assert_eq!(
            expiry_from("2026-01-31T12:00:00Z", "1d").unwrap(),
            "2026-02-01T12:00:00Z"
        );
        assert_eq!(
            expiry_from("2026-12-31T23:00:00Z", "2h").unwrap(),
            "2027-01-01T01:00:00Z"
        );
        // A leap year, which a naive day count gets wrong.
        assert_eq!(
            expiry_from("2028-02-28T00:00:00Z", "1d").unwrap(),
            "2028-02-29T00:00:00Z"
        );
    }

    #[test]
    fn a_lifetime_that_is_not_one_is_refused() {
        for bad in ["", "h", "0h", "-1h", "24", "24y", "twenty4h", "24 h"] {
            assert!(
                expiry_from("2026-09-13T00:00:00Z", bad).is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn an_invite_store_name_cannot_escape_the_store() {
        // Invite identifiers are base64url, but the name is built rather
        // than trusted, so this is worth pinning.
        let name = invite_store_name("iFeQifaw9tVbdMRboDaYqg");

        assert!(!name.contains('/'));
        assert!(!name.contains('\\'));
        assert!(!name.contains(".."));
        assert!(name.starts_with("invite-"));
    }
}

#[cfg(test)]
mod repository_tests {
    use super::canonical_repository;

    #[test]
    fn the_documented_shorthand_becomes_a_url_git_can_use() {
        // `--help` and PRD section 22.4 both ask for `owner/name`, and nothing
        // expanded it, so git resolved it as a relative path and the first
        // channel created from the documented form failed. Only a full URL
        // worked, which made the documentation wrong about its own CLI.
        assert_eq!(
            canonical_repository("czinegeroland/hrc-test"),
            "https://github.com/czinegeroland/hrc-test.git"
        );
        assert_eq!(
            canonical_repository("  owner/name  "),
            "https://github.com/owner/name.git"
        );
        assert_eq!(
            canonical_repository("owner/name.git"),
            "https://github.com/owner/name.git"
        );
    }

    #[test]
    fn anything_already_addressable_is_left_alone() {
        // A locator that already says where it points must survive untouched.
        // Rewriting one would silently move a channel to a different remote,
        // which is worse than refusing it.
        for locator in [
            "https://github.com/owner/name.git",
            "https://gitlab.com/owner/name.git",
            "ssh://git@github.com/owner/name.git",
            "git@github.com:owner/name.git",
        ] {
            assert_eq!(canonical_repository(locator), locator);
        }
    }

    #[test]
    fn a_local_repository_is_not_rewritten_into_a_github_url() {
        // A bare repository on disk is a legitimate transport, and the tests
        // in this crate use one. Expanding a path into a GitHub URL would
        // point a working channel at a repository nobody owns.
        for locator in [
            "/tmp/channel.git",
            "./channel.git",
            "../channel.git",
            "~/channels/one.git",
            "C:\\channels\\one.git",
            "\\\\server\\share\\one.git",
        ] {
            assert_eq!(canonical_repository(locator), locator);
        }
    }

    #[test]
    fn something_that_is_not_the_shorthand_is_passed_through() {
        // Guessing which two of three segments were meant is how a channel
        // ends up addressed to the wrong repository.
        for locator in ["owner", "owner/name/extra", "owner/", "/name"] {
            assert_eq!(canonical_repository(locator), locator);
        }
    }
}
