//! Transport adapter interface for Herdr Remote Channel.
//!
//! A transport moves opaque encrypted objects and nothing else. It never sees
//! plaintext, never holds a private key, and never interprets an object's
//! contents (PRD section 21.3). Everything it is trusted for is structural:
//! that a publication is atomic, that revisions form a linear chain, and that
//! bytes come back exactly as they went in.
//!
//! The core depends on this trait rather than on Git, which is what makes
//! transport independence real instead of aspirational. PRD section 21.3
//! requires an in-memory reference adapter for exactly that reason, and the
//! [`conformance`] suite runs against any adapter, so the Git transport and
//! the reference adapter are held to one standard.
//!
//! # Publication model
//!
//! Section 21.1.1 requires a linear, append-only sequence of atomic
//! publications:
//!
//! - Genesis is the only publication with no parent.
//! - Every later publication names exactly one parent.
//! - [`Transport::publish`] is a compare-and-swap: it succeeds against the
//!   expected revision or publishes nothing at all.
//! - [`Transport::fetch`] returns complete publications in order, each
//!   naming the previous one as its parent.
//!
//! A transport that cannot provide that evidence cannot host a channel with
//! mutable membership, because a client could not tell which roster state a
//! message was introduced under.

pub mod conformance;
pub mod memory;

use std::collections::BTreeMap;

/// An opaque, adapter-defined revision identifier.
///
/// For Git this is a commit SHA. The core never parses it, compares it for
/// ordering, or derives anything from it; it is only ever passed back.
pub type Revision = String;

/// What kind of publication this is.
///
/// The class is structural, not content-based: an adapter enforces that a
/// control publication carries only control objects without knowing what a
/// control object means (PRD section 16.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublicationClass {
    /// The channel's first publication. Only this one may have no parent.
    Genesis,
    /// Membership or policy change. Carries control objects only.
    Control,
    /// Joins, messages, blobs, or snapshots. Carries no control object.
    Data,
}

impl PublicationClass {
    /// The wire name of this class.
    pub const fn as_str(self) -> &'static str {
        match self {
            PublicationClass::Genesis => "genesis",
            PublicationClass::Control => "control",
            PublicationClass::Data => "data",
        }
    }
}

/// What kind of object this is within a publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObjectClass {
    /// A control-log entry.
    Control,
    /// The channel protocol descriptor published with genesis.
    Protocol,
    /// A join request.
    Join,
    /// A message.
    Message,
    /// A lazily fetched attachment.
    Blob,
    /// A roster snapshot.
    Snapshot,
}

impl ObjectClass {
    /// The wire name of this class.
    pub const fn as_str(self) -> &'static str {
        match self {
            ObjectClass::Control => "control",
            ObjectClass::Protocol => "protocol",
            ObjectClass::Join => "join",
            ObjectClass::Message => "message",
            ObjectClass::Blob => "blob",
            ObjectClass::Snapshot => "snapshot",
        }
    }

    /// Whether an object of this class belongs to a control publication.
    ///
    /// This is the rule that keeps roster epochs unambiguous: a commit that
    /// mixed a control entry with a message would leave it undefined which
    /// epoch that message was introduced under (PRD section 16.3).
    pub const fn is_control(self) -> bool {
        matches!(self, ObjectClass::Control | ObjectClass::Protocol)
    }
}

/// An object being published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishObject {
    /// The object's immutable name, which is also its location in the
    /// channel layout (PRD section 16.2), for example
    /// `messages/2026/09/01ARZ3.age`.
    ///
    /// Name and location are one field on purpose. Separating them gives an
    /// object two identities that adapters can disagree about: a
    /// content-addressed transport like Git has no way to store a name that
    /// differs from the path it lives at, while an adapter with its own
    /// index can ignore the path entirely. See decision DEC-027.
    pub name: String,
    /// What kind of object this is.
    pub class: ObjectClass,
    /// The bytes. Opaque ciphertext as far as the adapter is concerned.
    pub bytes: Vec<u8>,
}

/// An object as reported back by the transport.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectRecord {
    /// The object's immutable name and layout location.
    pub name: String,
    /// What kind of object this is.
    pub class: ObjectClass,
    /// Size in bytes.
    pub size: u64,
    /// Lowercase hex SHA-256 of the bytes.
    pub sha256: String,
}

/// One atomic publication.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Publication {
    /// This publication's revision.
    pub revision: Revision,
    /// The parent revision. `None` only for genesis.
    pub parent_revision: Option<Revision>,
    /// What kind of publication this is.
    pub class: PublicationClass,
    /// Adapter-reported time. Evidence, never semantic ordering.
    pub transport_time: String,
    /// Objects introduced by this publication.
    pub objects: Vec<ObjectRecord>,
}

/// The channel's current state as the transport sees it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupState {
    /// Adapter-defined group identifier.
    pub group_id: String,
    /// Current head revision, or `None` for an empty channel.
    pub revision: Option<Revision>,
    /// The history model. Only `linear_append_only` is supported.
    pub history_model: &'static str,
}

/// A page of fetched publications.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchPage {
    /// Publications in order, oldest first.
    pub publications: Vec<Publication>,
    /// Cursor to pass as `after` next time.
    pub cursor: Option<Revision>,
    /// Whether more publications are available beyond this page.
    pub more: bool,
    /// Anomalies the adapter observed, such as a rewritten history.
    pub anomalies: Vec<String>,
}

/// Properties an adapter must declare (PRD section 21.2).
///
/// The core reads these rather than assuming Git's behavior: an adapter with
/// a small object limit or no lazy blob support changes how the core batches
/// and fetches, and it must be able to find that out without special-casing
/// a provider by name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterCapabilities {
    /// Adapter identifier, for diagnostics.
    pub adapter: String,
    /// Transport protocol version implemented.
    pub protocol: String,
    /// Largest single object, in bytes.
    pub max_object_bytes: u64,
    /// Largest number of objects in one publication.
    pub max_objects_per_publication: usize,
    /// Whether published objects are durable once acknowledged.
    pub durable: bool,
    /// History model. Only `linear_append_only` is supported.
    pub history_model: &'static str,
    /// Whether the adapter can block until the revision changes.
    pub supports_wait: bool,
    /// Shortest polling interval the provider tolerates, in seconds.
    pub min_poll_interval_seconds: u32,
    /// Whether objects can be fetched individually and lazily.
    pub supports_lazy_objects: bool,
    /// Whether the adapter can create the group itself.
    pub supports_group_creation: bool,
}

/// Errors an adapter can report.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The expected revision was stale; nothing was published.
    ///
    /// This is the normal outcome of a race between two peers, not a
    /// failure. The caller rebuilds on `current` and retries.
    #[error("publication conflict: the channel has advanced to {current:?}")]
    Conflict {
        /// The revision the channel is actually at now.
        current: Option<Revision>,
    },

    /// The channel does not exist yet.
    #[error("channel does not exist in this transport")]
    NoSuchGroup,

    /// The channel already exists and cannot be created again.
    #[error("channel already exists in this transport")]
    GroupExists,

    /// A named object was not found.
    #[error("object {name} not found")]
    NoSuchObject {
        /// The requested name.
        name: String,
    },

    /// A fetched object did not match its expected hash.
    ///
    /// PRD section 26 treats a modified object as a security event, not a
    /// retryable error: the affected channel stops processing.
    #[error("object {name} does not match its expected hash")]
    ObjectHashMismatch {
        /// The object whose bytes changed.
        name: String,
    },

    /// A publication violated the structural rules.
    #[error("invalid publication: {reason}")]
    InvalidPublication {
        /// What was wrong.
        reason: String,
    },

    /// An object exceeded a declared limit.
    #[error("object {name} of {size} bytes exceeds the {limit} byte limit")]
    ObjectTooLarge {
        /// The oversized object.
        name: String,
        /// Its size.
        size: u64,
        /// The declared limit.
        limit: u64,
    },

    /// The underlying provider failed.
    #[error("transport provider error: {0}")]
    Provider(String),
}

/// Convenience alias for transport results.
pub type Result<T> = std::result::Result<T, TransportError>;

/// The history model every supported adapter must implement.
pub const LINEAR_APPEND_ONLY: &str = "linear_append_only";

/// A request to publish one atomic set of objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRequest {
    /// The revision the caller believes is current.
    ///
    /// `None` means "the channel is empty", which is only valid for genesis.
    pub expected_revision: Option<Revision>,
    /// What kind of publication this is.
    pub class: PublicationClass,
    /// The objects to add. Never empty.
    pub objects: Vec<PublishObject>,
}

/// A transport that can host one channel.
///
/// Implementations move opaque bytes. They do not decrypt, validate
/// signatures, or interpret object contents.
pub trait Transport {
    /// The properties this adapter guarantees.
    fn capabilities(&self) -> &AdapterCapabilities;

    /// Creates the channel and publishes genesis atomically.
    fn create_group(&mut self, objects: Vec<PublishObject>) -> Result<Publication>;

    /// Reports the channel's current head.
    fn open_group(&self) -> Result<GroupState>;

    /// Publishes atomically against `expected_revision`.
    ///
    /// Returns [`TransportError::Conflict`] without publishing anything if
    /// the channel has moved on.
    fn publish(&mut self, request: PublishRequest) -> Result<Publication>;

    /// Returns publications after `after`, oldest first.
    ///
    /// `after` of `None` starts at genesis. The first returned publication
    /// must descend directly from `after`, and each later one must name the
    /// preceding returned revision as its parent.
    fn fetch(&self, after: Option<&str>, limit: usize) -> Result<FetchPage>;

    /// Retrieves one object by name, verifying it against `expected_sha256`.
    fn get_object(&self, name: &str, expected_sha256: &str) -> Result<Vec<u8>>;

    /// Reports whether the adapter can reach its provider.
    fn health(&self) -> Result<()>;
}

/// Validates the structural rules a publication must satisfy.
///
/// Shared by every adapter so that "control publications carry only control
/// objects" is enforced identically rather than reimplemented per provider.
pub fn validate_publication(request: &PublishRequest) -> Result<()> {
    if request.objects.is_empty() {
        return Err(TransportError::InvalidPublication {
            reason: "a publication must contain at least one object".into(),
        });
    }

    let mut names = BTreeMap::new();
    for object in &request.objects {
        if names.insert(&object.name, ()).is_some() {
            return Err(TransportError::InvalidPublication {
                reason: format!("object name {} appears twice", object.name),
            });
        }
    }

    match request.class {
        PublicationClass::Control | PublicationClass::Genesis => {
            if let Some(object) = request.objects.iter().find(|o| !o.class.is_control()) {
                return Err(TransportError::InvalidPublication {
                    reason: format!(
                        "a {} publication cannot carry a {} object",
                        request.class.as_str(),
                        object.class.as_str()
                    ),
                });
            }
        }
        PublicationClass::Data => {
            if let Some(object) = request.objects.iter().find(|o| o.class.is_control()) {
                return Err(TransportError::InvalidPublication {
                    reason: format!(
                        "a data publication cannot carry a {} object",
                        object.class.as_str()
                    ),
                });
            }
        }
    }

    Ok(())
}
