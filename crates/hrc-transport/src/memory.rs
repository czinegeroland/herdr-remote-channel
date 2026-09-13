//! In-memory reference adapter.
//!
//! PRD section 21.3 requires this before the Git adapter counts as complete:
//! if the core only ever runs against Git, "transport independence" is a
//! claim rather than a property. This adapter shares no code with Git, so
//! anything the core needs that leaks Git-specific behavior fails here.
//!
//! It is also the fixture the conformance suite is developed against, and it
//! is deliberately strict: it enforces every structural rule rather than
//! being permissive, so a core bug surfaces here rather than in a provider.

use std::collections::BTreeMap;

use crate::{
    AdapterCapabilities, FetchPage, GroupState, LINEAR_APPEND_ONLY, ObjectRecord, Publication,
    PublicationClass, PublishObject, PublishRequest, Result, Revision, Transport, TransportError,
    validate_publication,
};

/// Largest object this adapter accepts, matching the PRD section 20.3 hard
/// maximum for a single attachment ciphertext.
const MAX_OBJECT_BYTES: u64 = 25 * 1024 * 1024;

/// An in-memory channel.
#[derive(Debug, Default)]
pub struct MemoryTransport {
    group_id: String,
    capabilities: Option<AdapterCapabilities>,
    /// Publications in order, oldest first.
    publications: Vec<Publication>,
    /// Object bytes by name.
    objects: BTreeMap<String, Vec<u8>>,
    /// Monotonic counter behind the synthetic revision identifiers.
    next_revision: u64,
    /// Synthetic clock, so transport times are deterministic in tests.
    next_time: u64,
}

impl MemoryTransport {
    /// Creates an empty transport with no channel yet.
    pub fn new(group_id: impl Into<String>) -> Self {
        let group_id = group_id.into();
        Self {
            capabilities: Some(AdapterCapabilities {
                adapter: "hrc-transport-memory".into(),
                protocol: hrc_protocol::TRANSPORT_PROTOCOL_ID.into(),
                max_object_bytes: MAX_OBJECT_BYTES,
                max_objects_per_publication: 1024,
                // Honest: this adapter loses everything when dropped, and a
                // core that assumes durability must not silently rely on it.
                durable: false,
                history_model: LINEAR_APPEND_ONLY,
                supports_wait: false,
                min_poll_interval_seconds: 0,
                supports_lazy_objects: true,
                supports_group_creation: true,
            }),
            group_id,
            ..Default::default()
        }
    }

    /// Whether the channel has been created.
    pub fn exists(&self) -> bool {
        !self.publications.is_empty()
    }

    /// The current head revision.
    pub fn head(&self) -> Option<Revision> {
        self.publications.last().map(|p| p.revision.clone())
    }

    /// Replaces an object's bytes without changing any revision.
    ///
    /// Not part of the [`Transport`] trait: no honest transport offers this.
    /// It exists so tests can simulate a hostile or corrupted provider and
    /// confirm the core detects substitution rather than trusting the name.
    pub fn corrupt_object_for_test(&mut self, name: &str, bytes: Vec<u8>) {
        self.objects.insert(name.to_owned(), bytes);
    }

    /// Drops the most recent publication, simulating a history rewrite.
    ///
    /// Also test-only, for the same reason.
    pub fn rewrite_history_for_test(&mut self) -> Option<Publication> {
        self.publications.pop()
    }

    /// Appends a validated publication and returns it.
    fn append(&mut self, class: PublicationClass, objects: Vec<PublishObject>) -> Publication {
        let parent_revision = self.head();

        self.next_revision += 1;
        self.next_time += 1;
        let revision = format!("rev-{:08}", self.next_revision);

        let records = objects
            .into_iter()
            .map(|object| {
                let record = ObjectRecord {
                    name: object.name.clone(),
                    class: object.class,
                    size: object.bytes.len() as u64,
                    sha256: hrc_protocol::canonical::sha256_hex(&object.bytes),
                };
                self.objects.insert(object.name, object.bytes);
                record
            })
            .collect();

        let publication = Publication {
            revision,
            parent_revision,
            class,
            transport_time: format!("2026-09-13T00:00:{:02}Z", self.next_time.min(59)),
            objects: records,
        };

        self.publications.push(publication.clone());
        publication
    }

    /// Rejects objects that break a declared limit or repeat a name.
    fn check_objects(&self, objects: &[PublishObject]) -> Result<()> {
        let capabilities = self.capabilities();

        if objects.len() > capabilities.max_objects_per_publication {
            return Err(TransportError::InvalidPublication {
                reason: format!(
                    "{} objects exceeds the {} object limit",
                    objects.len(),
                    capabilities.max_objects_per_publication
                ),
            });
        }

        for object in objects {
            let size = object.bytes.len() as u64;
            if size > capabilities.max_object_bytes {
                return Err(TransportError::ObjectTooLarge {
                    name: object.name.clone(),
                    size,
                    limit: capabilities.max_object_bytes,
                });
            }

            // Objects are immutable. Reusing a name with different bytes is
            // the substitution case from PRD section 17.4, and it is a
            // security conflict rather than an overwrite.
            if let Some(existing) = self.objects.get(&object.name)
                && existing != &object.bytes
            {
                return Err(TransportError::InvalidPublication {
                    reason: format!(
                        "object {} already exists with different content",
                        object.name
                    ),
                });
            }
        }

        Ok(())
    }
}

impl Transport for MemoryTransport {
    fn capabilities(&self) -> &AdapterCapabilities {
        self.capabilities
            .as_ref()
            .expect("capabilities are set in new()")
    }

    fn create_group(&mut self, objects: Vec<PublishObject>) -> Result<Publication> {
        if self.exists() {
            return Err(TransportError::GroupExists);
        }

        let request = PublishRequest {
            expected_revision: None,
            class: PublicationClass::Genesis,
            objects,
        };
        validate_publication(&request)?;
        self.check_objects(&request.objects)?;

        Ok(self.append(PublicationClass::Genesis, request.objects))
    }

    fn open_group(&self) -> Result<GroupState> {
        if !self.exists() {
            return Err(TransportError::NoSuchGroup);
        }

        Ok(GroupState {
            group_id: self.group_id.clone(),
            revision: self.head(),
            history_model: LINEAR_APPEND_ONLY,
        })
    }

    fn publish(&mut self, request: PublishRequest) -> Result<Publication> {
        if !self.exists() {
            return Err(TransportError::NoSuchGroup);
        }

        if request.class == PublicationClass::Genesis {
            return Err(TransportError::InvalidPublication {
                reason: "genesis can only be published by creating the channel".into(),
            });
        }

        // Compare-and-swap first: a stale caller must not have its objects
        // written, even partially, before the conflict is reported.
        if request.expected_revision != self.head() {
            return Err(TransportError::Conflict {
                current: self.head(),
            });
        }

        validate_publication(&request)?;
        self.check_objects(&request.objects)?;

        Ok(self.append(request.class, request.objects))
    }

    fn fetch(&self, after: Option<&str>, limit: usize) -> Result<FetchPage> {
        if !self.exists() {
            return Err(TransportError::NoSuchGroup);
        }

        let start = match after {
            None => 0,
            Some(revision) => {
                let position = self
                    .publications
                    .iter()
                    .position(|p| p.revision == revision)
                    .ok_or_else(|| TransportError::InvalidPublication {
                        // A cursor the transport cannot place means the
                        // client's view and the transport's have diverged.
                        reason: format!("cursor {revision} is not part of this history"),
                    })?;
                position + 1
            }
        };

        let end = self.publications.len().min(start + limit.max(1));
        let publications = self.publications[start..end].to_vec();
        let cursor = publications
            .last()
            .map(|p| p.revision.clone())
            .or_else(|| after.map(str::to_owned));

        Ok(FetchPage {
            more: end < self.publications.len(),
            publications,
            cursor,
            anomalies: Vec::new(),
        })
    }

    fn get_object(&self, name: &str, expected_sha256: &str) -> Result<Vec<u8>> {
        let bytes = self
            .objects
            .get(name)
            .ok_or_else(|| TransportError::NoSuchObject {
                name: name.to_owned(),
            })?;

        if hrc_protocol::canonical::sha256_hex(bytes) != expected_sha256 {
            return Err(TransportError::ObjectHashMismatch {
                name: name.to_owned(),
            });
        }

        Ok(bytes.clone())
    }

    fn health(&self) -> Result<()> {
        Ok(())
    }
}
