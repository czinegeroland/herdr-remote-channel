//! Synchronization: moving objects between the local store and a transport.
//!
//! Two loops, both driven by the daemon:
//!
//! - **Publish.** Take queued outbox records, publish them against the
//!   current tip, and on a conflict rebuild on the new tip and retry
//!   (PRD sections 12.5 and 17.3).
//! - **Fetch.** Pull publications after the durable cursor, apply control
//!   entries to the roster, quarantine messages, and advance the cursor
//!   only over what was fully processed (PRD section 12.5).
//!
//! Nothing here sleeps or reads a clock. Timing is expressed as *policy* —
//! [`poll_interval`] and [`Backoff`] return durations, and the caller waits.
//! That keeps the interesting behavior (when to retry, when to halt, what
//! the cursor should be) testable without real time passing, and leaves the
//! choice of runtime to the daemon.
//!
//! # Failing closed
//!
//! Anything that suggests the history a client was shown is not the history
//! it is being shown now — a rewritten chain, a substituted object, a cursor
//! the transport cannot place — stops synchronization for that channel and
//! records a sticky reason (PRD sections 17.5.2 and 26). Synchronization
//! does not resume on its own, because the safe response to "the record
//! changed underneath me" is never to keep reading.

use std::time::Duration;

use hrc_storage::Database;
use hrc_transport::{
    ObjectClass, Publication, PublicationClass, PublishObject, PublishRequest, Transport,
    TransportError,
};

use crate::error::{CoreError, Result};

/// How busy a channel is, which sets how often the daemon polls.
///
/// PRD section 17.1 gives the intervals. Polling faster than a provider
/// tolerates gets an installation rate-limited, and polling slower than the
/// user expects makes the product feel broken, so the interval follows what
/// the user is actually doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PollActivity {
    /// A caller is explicitly blocked on `hrc wait`.
    Waiting,
    /// A thread was active recently.
    ActiveThread,
    /// Ordinary background operation.
    Background,
    /// Nothing has happened for a long time.
    Idle,
}

/// The polling interval for a given activity level.
///
/// An adapter that declares a longer minimum wins: its provider's rate limit
/// is a hard constraint, and the PRD intervals are targets.
pub fn poll_interval(activity: PollActivity, adapter_minimum: Duration) -> Duration {
    let target = match activity {
        PollActivity::Waiting => Duration::from_secs(5),
        PollActivity::ActiveThread => Duration::from_secs(12),
        PollActivity::Background => Duration::from_secs(30),
        PollActivity::Idle => Duration::from_secs(300),
    };

    target.max(adapter_minimum)
}

/// Exponential backoff with jitter for failed transport operations.
///
/// PRD requirement HRC-SYNC-009. The jitter matters more than the growth:
/// several peers that failed against the same unavailable provider will
/// otherwise retry in lockstep and keep it unavailable.
#[derive(Debug, Clone, Copy)]
pub struct Backoff {
    base: Duration,
    ceiling: Duration,
}

impl Default for Backoff {
    fn default() -> Self {
        Self {
            base: Duration::from_secs(1),
            ceiling: Duration::from_secs(300),
        }
    }
}

impl Backoff {
    /// Builds a backoff policy.
    pub fn new(base: Duration, ceiling: Duration) -> Self {
        Self { base, ceiling }
    }

    /// The delay before attempt number `attempt`, counting from one.
    ///
    /// `jitter` is a fraction in `[0, 1)`, supplied by the caller so the
    /// policy stays deterministic under test. The result is in
    /// `[delay/2, delay)`, which spreads retries without ever collapsing to
    /// zero.
    pub fn delay_for(&self, attempt: u32, jitter: f64) -> Duration {
        let jitter = jitter.clamp(0.0, 1.0);

        let exponent = attempt.saturating_sub(1).min(16);
        let scaled = self
            .base
            .saturating_mul(2u32.saturating_pow(exponent))
            .min(self.ceiling);

        let half = scaled / 2;
        half + half.mul_f64(jitter)
    }
}

/// What a publish pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishOutcome {
    /// Messages confirmed present in the transport.
    pub published: Vec<String>,
    /// How many conflicts were resolved by rebuilding on a newer tip.
    pub conflicts: u32,
    /// Messages left queued because the attempt failed.
    pub deferred: Vec<String>,
}

/// The result of preparing one queued message against the current tip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PreparedPublication {
    /// Publish these bytes.
    Publish(PublishObject),
    /// The exact queued bytes are already present in canonical history.
    AlreadyPublished,
    /// Keep the message queued without attempting a publication.
    Defer,
}

/// What a fetch pass did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchOutcome {
    /// Publications processed.
    pub publications: usize,
    /// Control objects applied.
    pub control_objects: usize,
    /// Message objects quarantined.
    pub message_objects: usize,
    /// The cursor after this pass, if it advanced.
    pub cursor: Option<String>,
}

/// Publishes one queued outbox record, retrying on conflict.
///
/// A conflict is not a failure: another peer published first, so the caller
/// rebuilds on the reported tip and tries again. `max_attempts` bounds that,
/// because a channel busy enough to conflict indefinitely needs backoff
/// rather than a tighter loop.
pub fn publish_one<T: Transport>(
    transport: &mut T,
    database: &Database,
    channel_id: &str,
    message_id: &str,
    object: PublishObject,
    max_attempts: u32,
    now: &str,
) -> Result<PublishOutcome> {
    publish_one_prepared(
        transport,
        database,
        channel_id,
        message_id,
        move |_| Ok(PreparedPublication::Publish(object.clone())),
        max_attempts,
        now,
    )
}

/// Publishes one queued record, rebuilding its object against every observed tip.
///
/// `prepare` runs after the current tip is read and again after every
/// conflict. It can publish rebuilt bytes, confirm that a prior attempt
/// already reached canonical history, or defer without publishing. This is
/// the hook used to discard and re-encrypt stale-epoch ciphertext before a
/// commit is built.
pub fn publish_one_prepared<T, F>(
    transport: &mut T,
    database: &Database,
    channel_id: &str,
    message_id: &str,
    mut prepare: F,
    max_attempts: u32,
    now: &str,
) -> Result<PublishOutcome>
where
    T: Transport,
    F: FnMut(&T) -> Result<PreparedPublication>,
{
    let mut conflicts = 0;

    for _ in 0..max_attempts.max(1) {
        let head = match transport.open_group() {
            Ok(state) => state.revision,
            Err(error) => return Err(halt(database, channel_id, error, now)),
        };
        let object = match prepare(transport)? {
            PreparedPublication::Publish(object) => object,
            PreparedPublication::AlreadyPublished => {
                database.mark_published(message_id, now)?;
                return Ok(PublishOutcome {
                    published: vec![message_id.to_owned()],
                    conflicts,
                    deferred: Vec::new(),
                });
            }
            PreparedPublication::Defer => {
                return Ok(PublishOutcome {
                    published: Vec::new(),
                    conflicts,
                    deferred: vec![message_id.to_owned()],
                });
            }
        };

        let request = PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![object],
        };

        match transport.publish(request) {
            Ok(_) => {
                database.mark_published(message_id, now)?;
                return Ok(PublishOutcome {
                    published: vec![message_id.to_owned()],
                    conflicts,
                    deferred: Vec::new(),
                });
            }

            // Another peer got there first. Loop: the next iteration reads
            // the new tip and rebuilds on it.
            Err(TransportError::Conflict { .. }) => {
                conflicts += 1;
            }

            // An object that already exists with different bytes, a
            // rewritten history, or a substituted object is a security
            // event, not something to retry.
            Err(
                error @ (TransportError::InvalidPublication { .. }
                | TransportError::ObjectHashMismatch { .. }),
            ) => {
                return Err(halt(database, channel_id, error, now));
            }

            Err(error) => {
                database.record_attempt_failure(message_id, &error.to_string(), now)?;
                return Ok(PublishOutcome {
                    published: Vec::new(),
                    conflicts,
                    deferred: vec![message_id.to_owned()],
                });
            }
        }
    }

    database.record_attempt_failure(message_id, "exhausted conflict retries", now)?;
    Ok(PublishOutcome {
        published: Vec::new(),
        conflicts,
        deferred: vec![message_id.to_owned()],
    })
}

/// Fetches publications after the stored cursor and reports what arrived.
///
/// The cursor advances only over publications that were fully processed, so
/// an interruption re-reads rather than skips. Re-reading is safe because
/// message identifiers are deduplicated; skipping would silently drop a
/// roster change.
pub fn fetch_once<T: Transport>(
    transport: &T,
    database: &Database,
    channel_id: &str,
    limit: usize,
    now: &str,
) -> Result<FetchOutcome> {
    let channel = database
        .channel(channel_id)?
        .ok_or_else(|| CoreError::UnknownChannelState {
            channel_id: channel_id.to_owned(),
        })?;

    if let Some(reason) = channel.halted_reason {
        return Err(CoreError::SynchronizationHalted { reason });
    }

    let page = match transport.fetch(channel.sync_cursor.as_deref(), limit) {
        Ok(page) => page,
        Err(error) => return Err(halt(database, channel_id, error, now)),
    };

    // An adapter that reports an anomaly has seen something it cannot
    // reconcile. Believe it.
    if let Some(anomaly) = page.anomalies.first() {
        return Err(halt(
            database,
            channel_id,
            TransportError::Provider(anomaly.clone()),
            now,
        ));
    }

    verify_chain(&page.publications, channel.sync_cursor.as_deref())
        .map_err(|error| halt_synchronization(database, channel_id, error, now))?;

    let mut outcome = FetchOutcome {
        publications: page.publications.len(),
        control_objects: 0,
        message_objects: 0,
        cursor: None,
    };

    for publication in &page.publications {
        for object in &publication.objects {
            if object.class.is_control() {
                outcome.control_objects += 1;
            } else if object.class == ObjectClass::Message {
                outcome.message_objects += 1;
            }
        }

        // Advance one publication at a time so an interruption resumes at
        // the last fully processed point rather than at the page boundary.
        database.set_sync_cursor(channel_id, &publication.revision)?;
        outcome.cursor = Some(publication.revision.clone());
    }

    Ok(outcome)
}

/// Checks that a fetched page is an unbroken chain from the cursor.
///
/// The adapter is supposed to guarantee this, and the conformance suite
/// checks that it does. Verifying again here is deliberate: this is the
/// property that lets a client trust which roster epoch governs a message,
/// and a transport is exactly the component that might be compromised.
fn verify_chain(publications: &[Publication], after: Option<&str>) -> Result<()> {
    let mut expected_parent = after.map(str::to_owned);

    for publication in publications {
        match (&expected_parent, &publication.parent_revision) {
            (None, None) => {
                if publication.class != PublicationClass::Genesis {
                    return Err(CoreError::BrokenChain {
                        expected: "genesis".into(),
                        found: publication.class.as_str().into(),
                    });
                }
            }
            (Some(expected), Some(found)) if expected == found => {}
            (expected, found) => {
                return Err(CoreError::BrokenChain {
                    expected: expected.clone().unwrap_or_else(|| "genesis".into()),
                    found: found.clone().unwrap_or_else(|| "none".into()),
                });
            }
        }

        expected_parent = Some(publication.revision.clone());
    }

    Ok(())
}

/// Halts a channel and returns the error that caused it.
fn halt(database: &Database, channel_id: &str, error: TransportError, now: &str) -> CoreError {
    halt_synchronization(
        database,
        channel_id,
        CoreError::Transport(error.to_string()),
        now,
    )
}

/// Halts a channel for an already-built core error.
///
/// Transport integrations call this when validation outside the generic
/// fetch loop finds an integrity failure. Keeping the state transition here
/// ensures every halt is sticky and audited in the same way.
pub fn halt_synchronization(
    database: &Database,
    channel_id: &str,
    error: CoreError,
    now: &str,
) -> CoreError {
    let reason = error.to_string();

    // The halt itself must be recorded even if the audit write fails, so
    // report the original cause rather than a bookkeeping failure.
    let _ = database.halt_channel(channel_id, &reason);
    let _ = database.append_audit(
        Some(channel_id),
        None,
        "synchronization_halted",
        None,
        Some(&reason),
        now,
    );

    error
}

#[cfg(test)]
mod tests;
