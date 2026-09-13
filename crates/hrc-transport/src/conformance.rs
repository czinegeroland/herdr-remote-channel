//! The adapter conformance suite.
//!
//! PRD requirement HRC-TR-007: adapters must be validated against a published
//! suite, and the in-memory reference adapter must pass it before the Git
//! adapter is considered complete. Keeping the suite here — in the crate that
//! defines the contract, not in a test directory of one implementation —
//! means every adapter is held to the same standard, including ones written
//! outside this repository.
//!
//! Each check states the property it enforces and why the protocol needs it.
//! An adapter author reading a failure should learn what breaks if the
//! behavior is wrong, not just that an assertion failed.
//!
//! ```no_run
//! use hrc_transport::conformance;
//! use hrc_transport::memory::MemoryTransport;
//!
//! conformance::run_suite(|| MemoryTransport::new("test")).expect("adapter conforms");
//! ```

use crate::{
    LINEAR_APPEND_ONLY, ObjectClass, PublicationClass, PublishObject, PublishRequest, Transport,
    TransportError,
};

/// A conformance failure: which check failed, and what the protocol needs.
#[derive(Debug, thiserror::Error)]
#[error("conformance check `{check}` failed: {detail}\n  why it matters: {rationale}")]
pub struct ConformanceFailure {
    /// The check that failed.
    pub check: &'static str,
    /// What was observed.
    pub detail: String,
    /// What the protocol relies on this behavior for.
    pub rationale: &'static str,
}

/// Result of running the suite.
pub type Result<T> = std::result::Result<T, ConformanceFailure>;

/// Fails a check.
fn fail<T>(check: &'static str, rationale: &'static str, detail: impl Into<String>) -> Result<T> {
    Err(ConformanceFailure {
        check,
        rationale,
        detail: detail.into(),
    })
}

/// Asserts a condition, or fails the named check.
fn require(
    condition: bool,
    check: &'static str,
    rationale: &'static str,
    detail: impl Into<String>,
) -> Result<()> {
    if condition {
        Ok(())
    } else {
        fail(check, rationale, detail)
    }
}

/// Builds a control object with distinct bytes.
fn control_object(name: &str) -> PublishObject {
    PublishObject {
        name: format!("control/log/{name}.json"),
        class: ObjectClass::Control,
        bytes: format!("control-{name}").into_bytes(),
    }
}

/// Builds a message object with distinct bytes.
fn message_object(name: &str) -> PublishObject {
    PublishObject {
        name: format!("messages/2026/09/{name}.age"),
        class: ObjectClass::Message,
        bytes: format!("ciphertext-{name}").into_bytes(),
    }
}

/// Runs every conformance check against adapters produced by `factory`.
///
/// The factory is called repeatedly so each check starts from a fresh,
/// empty transport; checks that share state would mask ordering bugs.
pub fn run_suite<T: Transport, F: FnMut() -> T>(mut factory: F) -> Result<()> {
    capabilities_are_declared(&factory())?;
    an_empty_channel_cannot_be_opened(&factory())?;
    genesis_is_created_with_no_parent(&mut factory())?;
    genesis_cannot_be_created_twice(&mut factory())?;
    publications_form_a_linear_chain(&mut factory())?;
    publish_is_compare_and_swap(&mut factory())?;
    a_conflict_publishes_nothing(&mut factory())?;
    fetch_returns_genesis_first(&mut factory())?;
    fetch_returns_an_unbroken_ordered_chain(&mut factory())?;
    fetch_paginates_without_skipping(&mut factory())?;
    objects_round_trip_byte_exactly(&mut factory())?;
    object_hashes_are_verified(&mut factory())?;
    control_and_data_are_never_mixed(&mut factory())?;
    an_empty_publication_is_rejected(&mut factory())?;
    genesis_cannot_be_republished(&mut factory())?;
    Ok(())
}

fn capabilities_are_declared<T: Transport>(transport: &T) -> Result<()> {
    const CHECK: &str = "capabilities_are_declared";
    const WHY: &str = "The core batches, polls, and fetches according to \
                       declared limits. An adapter that does not declare them \
                       forces the core to assume Git's behavior, which is \
                       exactly the coupling the adapter interface removes.";

    let capabilities = transport.capabilities();
    require(
        !capabilities.adapter.is_empty(),
        CHECK,
        WHY,
        "adapter name is empty",
    )?;
    require(
        capabilities.history_model == LINEAR_APPEND_ONLY,
        CHECK,
        WHY,
        format!(
            "history model is {}, but only {LINEAR_APPEND_ONLY} is supported",
            capabilities.history_model
        ),
    )?;
    require(
        capabilities.max_object_bytes > 0,
        CHECK,
        WHY,
        "maximum object size is zero",
    )?;
    require(
        capabilities.max_objects_per_publication > 0,
        CHECK,
        WHY,
        "maximum objects per publication is zero",
    )
}

fn an_empty_channel_cannot_be_opened<T: Transport>(transport: &T) -> Result<()> {
    const CHECK: &str = "an_empty_channel_cannot_be_opened";
    const WHY: &str = "A channel begins at genesis. Reporting a head for a \
                       channel that has none would let a client treat an \
                       empty transport as a valid, if unusual, history.";

    match transport.open_group() {
        Err(TransportError::NoSuchGroup) => Ok(()),
        other => fail(CHECK, WHY, format!("expected NoSuchGroup, got {other:?}")),
    }
}

fn genesis_is_created_with_no_parent<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "genesis_is_created_with_no_parent";
    const WHY: &str = "Genesis is the only publication permitted to have no \
                       parent. It is the anchor a client validates the rest \
                       of the history against.";

    let publication = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;

    require(
        publication.parent_revision.is_none(),
        CHECK,
        WHY,
        "genesis reported a parent revision",
    )?;
    require(
        publication.class == PublicationClass::Genesis,
        CHECK,
        WHY,
        format!("genesis was classed as {}", publication.class.as_str()),
    )?;

    let state = transport.open_group().map_err(|error| ConformanceFailure {
        check: CHECK,
        rationale: WHY,
        detail: format!("open_group failed after creation: {error}"),
    })?;
    require(
        state.revision.as_deref() == Some(publication.revision.as_str()),
        CHECK,
        WHY,
        "open_group did not report genesis as the head",
    )
}

fn genesis_cannot_be_created_twice<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "genesis_cannot_be_created_twice";
    const WHY: &str = "Two genesis publications would be two channels sharing \
                       one location, and a client could not tell which \
                       history its channel ID refers to.";

    transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("first create_group failed: {error}"),
        })?;

    match transport.create_group(vec![control_object("00000000b")]) {
        Err(TransportError::GroupExists) => Ok(()),
        other => fail(CHECK, WHY, format!("expected GroupExists, got {other:?}")),
    }
}

fn publications_form_a_linear_chain<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "publications_form_a_linear_chain";
    const WHY: &str = "Each publication naming exactly one parent is what \
                       lets a client establish where a message was introduced \
                       and therefore which roster epoch governs it.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;

    let next = transport
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message_object("msg-1")],
        })
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("publish failed: {error}"),
        })?;

    require(
        next.parent_revision.as_deref() == Some(genesis.revision.as_str()),
        CHECK,
        WHY,
        format!(
            "expected parent {}, got {:?}",
            genesis.revision, next.parent_revision
        ),
    )?;
    require(
        next.revision != genesis.revision,
        CHECK,
        WHY,
        "the new publication reused the genesis revision",
    )
}

fn publish_is_compare_and_swap<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "publish_is_compare_and_swap";
    const WHY: &str = "Two peers publish concurrently by design. Without a \
                       compare-and-swap the loser would silently overwrite \
                       or fork the winner's history instead of retrying.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;

    transport
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message_object("msg-1")],
        })
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("first publish failed: {error}"),
        })?;

    // A second peer still believes genesis is the head.
    match transport.publish(PublishRequest {
        expected_revision: Some(genesis.revision.clone()),
        class: PublicationClass::Data,
        objects: vec![message_object("msg-2")],
    }) {
        Err(TransportError::Conflict { current }) => require(
            current.is_some() && current.as_deref() != Some(genesis.revision.as_str()),
            CHECK,
            WHY,
            "the conflict did not report the advanced revision",
        ),
        other => fail(CHECK, WHY, format!("expected Conflict, got {other:?}")),
    }
}

fn a_conflict_publishes_nothing<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "a_conflict_publishes_nothing";
    const WHY: &str = "Publication is all or nothing. A conflict that left \
                       objects behind would put content in the channel that \
                       no publication accounts for.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;
    let head = transport
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message_object("msg-1")],
        })
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("publish failed: {error}"),
        })?;

    let orphan = message_object("orphan");
    let _ = transport.publish(PublishRequest {
        expected_revision: Some(genesis.revision),
        class: PublicationClass::Data,
        objects: vec![orphan.clone()],
    });

    let state = transport.open_group().map_err(|error| ConformanceFailure {
        check: CHECK,
        rationale: WHY,
        detail: format!("open_group failed: {error}"),
    })?;
    require(
        state.revision.as_deref() == Some(head.revision.as_str()),
        CHECK,
        WHY,
        "the head moved despite the conflict",
    )?;

    let expected = hrc_protocol::canonical::sha256_hex(&orphan.bytes);
    match transport.get_object(&orphan.name, &expected) {
        Err(TransportError::NoSuchObject { .. }) => Ok(()),
        other => fail(
            CHECK,
            WHY,
            format!("the rejected object was still stored: {other:?}"),
        ),
    }
}

fn fetch_returns_genesis_first<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "fetch_returns_genesis_first";
    const WHY: &str = "A joining client validates from genesis outward. If a \
                       fetch from the beginning omitted genesis, the client \
                       would be validating against an unanchored history.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;
    transport
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision.clone()),
            class: PublicationClass::Data,
            objects: vec![message_object("msg-1")],
        })
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("publish failed: {error}"),
        })?;

    let page = transport
        .fetch(None, 100)
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("fetch failed: {error}"),
        })?;

    let first = page.publications.first().ok_or(ConformanceFailure {
        check: CHECK,
        rationale: WHY,
        detail: "fetch returned nothing".into(),
    })?;

    require(
        first.revision == genesis.revision && first.parent_revision.is_none(),
        CHECK,
        WHY,
        "the first fetched publication was not genesis",
    )
}

fn fetch_returns_an_unbroken_ordered_chain<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "fetch_returns_an_unbroken_ordered_chain";
    const WHY: &str = "Each returned publication must name the previous one \
                       as its parent. A gap would let a control entry be \
                       missed, and a client would then evaluate later \
                       messages against a stale roster.";

    let mut head = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?
        .revision;

    for index in 1..=4 {
        head = transport
            .publish(PublishRequest {
                expected_revision: Some(head.clone()),
                class: PublicationClass::Data,
                objects: vec![message_object(&format!("msg-{index}"))],
            })
            .map_err(|error| ConformanceFailure {
                check: CHECK,
                rationale: WHY,
                detail: format!("publish {index} failed: {error}"),
            })?
            .revision;
    }

    let page = transport
        .fetch(None, 100)
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("fetch failed: {error}"),
        })?;

    require(
        page.publications.len() == 5,
        CHECK,
        WHY,
        format!("expected 5 publications, got {}", page.publications.len()),
    )?;

    for pair in page.publications.windows(2) {
        require(
            pair[1].parent_revision.as_deref() == Some(pair[0].revision.as_str()),
            CHECK,
            WHY,
            format!(
                "{} names parent {:?}, expected {}",
                pair[1].revision, pair[1].parent_revision, pair[0].revision
            ),
        )?;
    }

    Ok(())
}

fn fetch_paginates_without_skipping<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "fetch_paginates_without_skipping";
    const WHY: &str = "Cursors must never silently skip a publication. A \
                       skipped control entry is a roster change the client \
                       never learns about.";

    let mut head = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?
        .revision;

    let mut expected = vec![head.clone()];
    for index in 1..=5 {
        head = transport
            .publish(PublishRequest {
                expected_revision: Some(head.clone()),
                class: PublicationClass::Data,
                objects: vec![message_object(&format!("msg-{index}"))],
            })
            .map_err(|error| ConformanceFailure {
                check: CHECK,
                rationale: WHY,
                detail: format!("publish {index} failed: {error}"),
            })?
            .revision;
        expected.push(head.clone());
    }

    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    loop {
        let page = transport
            .fetch(cursor.as_deref(), 2)
            .map_err(|error| ConformanceFailure {
                check: CHECK,
                rationale: WHY,
                detail: format!("paged fetch failed: {error}"),
            })?;

        seen.extend(page.publications.iter().map(|p| p.revision.clone()));
        cursor = page.cursor;

        if !page.more {
            break;
        }
        require(
            cursor.is_some(),
            CHECK,
            WHY,
            "more publications were reported but no cursor was returned",
        )?;
    }

    require(
        seen == expected,
        CHECK,
        WHY,
        format!("paged fetch returned {seen:?}, expected {expected:?}"),
    )
}

fn objects_round_trip_byte_exactly<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "objects_round_trip_byte_exactly";
    const WHY: &str = "Objects are ciphertext. A transport that re-encodes, \
                       normalizes, or trims bytes destroys both decryption \
                       and the hashes the core verifies.";

    // Bytes chosen to break anything that assumes text: NUL, high bytes, and
    // sequences that are not valid UTF-8.
    let awkward = vec![0u8, 0xff, 0xfe, b'\n', b'\r', 0x80, 0x00, b'{', 0xc3];
    let object = PublishObject {
        name: "blobs/blob-1.age".into(),
        class: ObjectClass::Blob,
        bytes: awkward.clone(),
    };

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;
    let publication = transport
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision),
            class: PublicationClass::Data,
            objects: vec![object.clone()],
        })
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("publish failed: {error}"),
        })?;

    let record = publication.objects.first().ok_or(ConformanceFailure {
        check: CHECK,
        rationale: WHY,
        detail: "the publication reported no objects".into(),
    })?;
    require(
        record.size == awkward.len() as u64,
        CHECK,
        WHY,
        format!("reported size {} for {} bytes", record.size, awkward.len()),
    )?;

    let fetched = transport
        .get_object(&object.name, &record.sha256)
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("get_object failed: {error}"),
        })?;

    require(
        fetched == awkward,
        CHECK,
        WHY,
        "the returned bytes differ from the published bytes",
    )
}

fn object_hashes_are_verified<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "object_hashes_are_verified";
    const WHY: &str = "The core asks for an object by name and expected hash. \
                       An adapter that ignores the hash would let a provider \
                       substitute content under a name the core already \
                       trusts.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;
    transport
        .publish(PublishRequest {
            expected_revision: Some(genesis.revision),
            class: PublicationClass::Data,
            objects: vec![message_object("msg-1")],
        })
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("publish failed: {error}"),
        })?;

    let wrong = hrc_protocol::canonical::sha256_hex(b"not the object");
    match transport.get_object("messages/2026/09/msg-1.age", &wrong) {
        Err(TransportError::ObjectHashMismatch { .. }) => {}
        other => {
            return fail(
                CHECK,
                WHY,
                format!("expected ObjectHashMismatch, got {other:?}"),
            );
        }
    }

    match transport.get_object("does-not-exist", &wrong) {
        Err(TransportError::NoSuchObject { .. }) => Ok(()),
        other => fail(CHECK, WHY, format!("expected NoSuchObject, got {other:?}")),
    }
}

fn control_and_data_are_never_mixed<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "control_and_data_are_never_mixed";
    const WHY: &str = "PRD section 16.3: a publication carrying both a \
                       control entry and a message leaves it undefined which \
                       roster epoch that message was introduced under. That \
                       ambiguity is what a removed device would exploit.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;

    let mixed = transport.publish(PublishRequest {
        expected_revision: Some(genesis.revision.clone()),
        class: PublicationClass::Control,
        objects: vec![control_object("00000001"), message_object("msg-1")],
    });
    require(
        matches!(mixed, Err(TransportError::InvalidPublication { .. })),
        CHECK,
        WHY,
        format!("a mixed control publication was accepted: {mixed:?}"),
    )?;

    let control_in_data = transport.publish(PublishRequest {
        expected_revision: Some(genesis.revision),
        class: PublicationClass::Data,
        objects: vec![control_object("00000001")],
    });
    require(
        matches!(
            control_in_data,
            Err(TransportError::InvalidPublication { .. })
        ),
        CHECK,
        WHY,
        format!("a control object in a data publication was accepted: {control_in_data:?}"),
    )
}

fn an_empty_publication_is_rejected<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "an_empty_publication_is_rejected";
    const WHY: &str = "An empty publication advances the revision without \
                       adding anything, which would let a peer move the head \
                       a client is chaining against for no reason.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;

    let empty = transport.publish(PublishRequest {
        expected_revision: Some(genesis.revision),
        class: PublicationClass::Data,
        objects: Vec::new(),
    });

    require(
        matches!(empty, Err(TransportError::InvalidPublication { .. })),
        CHECK,
        WHY,
        format!("an empty publication was accepted: {empty:?}"),
    )
}

fn genesis_cannot_be_republished<T: Transport>(transport: &mut T) -> Result<()> {
    const CHECK: &str = "genesis_cannot_be_republished";
    const WHY: &str = "Genesis defines the channel. Allowing a later \
                       publication to claim the genesis class would let a \
                       second anchor appear in an existing history.";

    let genesis = transport
        .create_group(vec![control_object("00000000")])
        .map_err(|error| ConformanceFailure {
            check: CHECK,
            rationale: WHY,
            detail: format!("create_group failed: {error}"),
        })?;

    let again = transport.publish(PublishRequest {
        expected_revision: Some(genesis.revision),
        class: PublicationClass::Genesis,
        objects: vec![control_object("00000001")],
    });

    require(
        matches!(again, Err(TransportError::InvalidPublication { .. })),
        CHECK,
        WHY,
        format!("a second genesis publication was accepted: {again:?}"),
    )
}
