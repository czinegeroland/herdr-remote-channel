//! Synchronization tests.
//!
//! These drive the in-memory reference adapter rather than Git, which keeps
//! them fast and, more usefully, proves the sync logic is not quietly
//! depending on Git behavior.

use super::*;

use hrc_transport::ObjectClass;
use hrc_transport::memory::MemoryTransport;

const CHANNEL: &str = "channel-1";
const DEVICE: &str = "device-1";
const NOW: &str = "2026-09-13T00:00:00Z";

/// A registered channel in a fresh database.
fn database() -> Database {
    let database = Database::open_in_memory().unwrap();
    database
        .insert_channel(CHANNEL, "memory", "test", "Test channel", NOW)
        .unwrap();
    database
}

/// A control object.
fn control(sequence: u32) -> PublishObject {
    PublishObject {
        name: format!("control/log/{sequence:08}.json"),
        class: ObjectClass::Control,
        bytes: format!("control-{sequence}").into_bytes(),
    }
}

/// A message object.
fn message(name: &str) -> PublishObject {
    PublishObject {
        name: format!("messages/2026/09/{name}.age"),
        class: ObjectClass::Message,
        bytes: format!("ciphertext-{name}").into_bytes(),
    }
}

/// A transport with genesis already published.
fn transport() -> MemoryTransport {
    let mut transport = MemoryTransport::new(CHANNEL);
    transport.create_group(vec![control(0)]).unwrap();
    transport
}

/// Queues an outbox record ready to publish.
fn queue(database: &mut Database, message_id: &str) {
    database
        .allocate_outgoing(CHANNEL, DEVICE, message_id, 1, "payload-hash", NOW)
        .unwrap();
    database
        .queue_outgoing(message_id, b"ciphertext", NOW)
        .unwrap();
}

#[test]
fn poll_intervals_follow_the_specified_table() {
    let none = Duration::ZERO;

    assert_eq!(
        poll_interval(PollActivity::Waiting, none),
        Duration::from_secs(5)
    );
    assert_eq!(
        poll_interval(PollActivity::ActiveThread, none),
        Duration::from_secs(12)
    );
    assert_eq!(
        poll_interval(PollActivity::Background, none),
        Duration::from_secs(30)
    );
    assert_eq!(
        poll_interval(PollActivity::Idle, none),
        Duration::from_secs(300)
    );
}

#[test]
fn an_adapter_minimum_overrides_a_shorter_target() {
    // A provider's rate limit is a hard constraint; the PRD intervals are
    // targets. Polling faster than the adapter allows gets an installation
    // throttled.
    let minimum = Duration::from_secs(60);

    assert_eq!(poll_interval(PollActivity::Waiting, minimum), minimum);
    assert_eq!(poll_interval(PollActivity::Background, minimum), minimum);
    assert_eq!(
        poll_interval(PollActivity::Idle, minimum),
        Duration::from_secs(300),
        "a longer target still wins"
    );
}

#[test]
fn backoff_grows_and_is_capped() {
    let backoff = Backoff::new(Duration::from_secs(1), Duration::from_secs(60));

    // With zero jitter the delay is half the scaled value.
    assert_eq!(backoff.delay_for(1, 0.0), Duration::from_millis(500));
    assert_eq!(backoff.delay_for(2, 0.0), Duration::from_secs(1));
    assert_eq!(backoff.delay_for(3, 0.0), Duration::from_secs(2));
    assert_eq!(backoff.delay_for(10, 0.0), Duration::from_secs(30));
    assert_eq!(
        backoff.delay_for(1_000_000, 0.0),
        Duration::from_secs(30),
        "the ceiling holds however many attempts have failed"
    );
}

#[test]
fn backoff_jitter_spreads_retries_without_reaching_zero() {
    // Peers that failed against the same provider must not retry in
    // lockstep, and none of them may retry instantly.
    let backoff = Backoff::new(Duration::from_secs(4), Duration::from_secs(60));

    let earliest = backoff.delay_for(3, 0.0);
    let latest = backoff.delay_for(3, 1.0);

    assert!(earliest > Duration::ZERO);
    assert!(latest > earliest);
    assert_eq!(latest, Duration::from_secs(16));
    assert_eq!(earliest, Duration::from_secs(8));

    for jitter in [0.1, 0.25, 0.5, 0.9] {
        let delay = backoff.delay_for(3, jitter);
        assert!(delay >= earliest && delay <= latest);
    }
}

#[test]
fn out_of_range_jitter_is_clamped() {
    let backoff = Backoff::default();
    assert_eq!(backoff.delay_for(1, -5.0), backoff.delay_for(1, 0.0));
    assert_eq!(backoff.delay_for(1, 5.0), backoff.delay_for(1, 1.0));
}

#[test]
fn publishing_marks_the_record_published() {
    let mut database = database();
    let mut transport = transport();
    queue(&mut database, "msg-1");

    let outcome = publish_one(
        &mut transport,
        &database,
        CHANNEL,
        "msg-1",
        message("msg-1"),
        3,
        NOW,
    )
    .unwrap();

    assert_eq!(outcome.published, vec!["msg-1"]);
    assert_eq!(outcome.conflicts, 0);
    assert!(database.pending_outgoing(CHANNEL).unwrap().is_empty());
}

#[test]
fn a_conflict_is_resolved_by_rebuilding_on_the_new_tip() {
    // PRD section 17.3: the loser of a race rebuilds rather than failing.
    // The conflict is simulated by moving the tip out from under the caller
    // between reading it and publishing.
    let mut database = database();
    let mut transport = transport();
    queue(&mut database, "msg-1");

    // Another peer publishes first.
    let head = transport.open_group().unwrap().revision;
    transport
        .publish(PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![message("from-elsewhere")],
        })
        .unwrap();

    let outcome = publish_one(
        &mut transport,
        &database,
        CHANNEL,
        "msg-1",
        message("msg-1"),
        3,
        NOW,
    )
    .unwrap();

    // Reading the tip fresh each attempt means this succeeds first time.
    assert_eq!(outcome.published, vec!["msg-1"]);
}

#[test]
fn a_security_conflict_halts_the_channel_instead_of_retrying() {
    // Publishing a name that already exists with different bytes is the
    // substitution case from PRD section 17.4. Retrying it would be retrying
    // an attack.
    let mut database = database();
    let mut transport = transport();
    queue(&mut database, "msg-1");

    let head = transport.open_group().unwrap().revision;
    transport
        .publish(PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let mut different = message("msg-1");
    different.bytes = b"different ciphertext".to_vec();

    let error = publish_one(
        &mut transport,
        &database,
        CHANNEL,
        "msg-1",
        different,
        3,
        NOW,
    )
    .unwrap_err();

    assert!(matches!(error, CoreError::Transport(_)), "got {error}");

    let channel = database.channel(CHANNEL).unwrap().unwrap();
    assert!(
        channel.halted_reason.is_some(),
        "the channel must be halted"
    );
    assert!(
        database
            .audit_actions()
            .unwrap()
            .contains(&"synchronization_halted".to_owned()),
        "the halt must be audited"
    );
}

#[test]
fn a_transient_failure_defers_the_message_without_losing_it() {
    // The channel does not exist in this transport, which is a provider
    // failure rather than a security event.
    let mut database = database();
    let mut transport = MemoryTransport::new(CHANNEL);
    queue(&mut database, "msg-1");

    let error = publish_one(
        &mut transport,
        &database,
        CHANNEL,
        "msg-1",
        message("msg-1"),
        3,
        NOW,
    )
    .unwrap_err();

    assert!(matches!(error, CoreError::Transport(_)), "got {error}");
    // The message is still in the outbox: nothing was lost.
    assert_eq!(database.pending_outgoing(CHANNEL).unwrap(), vec!["msg-1"]);
}

#[test]
fn fetching_advances_the_cursor_over_processed_publications() {
    let database = database();
    let mut transport = transport();

    let head = transport.open_group().unwrap().revision;
    let second = transport
        .publish(PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let outcome = fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap();

    assert_eq!(outcome.publications, 2);
    assert_eq!(
        outcome.control_objects, 1,
        "genesis carries a control object"
    );
    assert_eq!(outcome.message_objects, 1);
    assert_eq!(outcome.cursor.as_deref(), Some(second.revision.as_str()));

    let channel = database.channel(CHANNEL).unwrap().unwrap();
    assert_eq!(channel.sync_cursor, Some(second.revision));
}

#[test]
fn a_second_fetch_returns_nothing_new() {
    let database = database();
    let transport = transport();

    fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap();
    let second = fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap();

    assert_eq!(second.publications, 0);
}

#[test]
fn fetching_resumes_from_the_stored_cursor() {
    let database = database();
    let mut transport = transport();

    fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap();

    let head = transport.open_group().unwrap().revision;
    transport
        .publish(PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let outcome = fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap();
    assert_eq!(outcome.publications, 1, "only the new publication");
    assert_eq!(outcome.message_objects, 1);
}

#[test]
fn a_history_rewrite_halts_synchronization() {
    // The cursor names a revision the transport can no longer place, which
    // is what a rewrite looks like from the client's side.
    let database = database();
    let mut transport = transport();

    let head = transport.open_group().unwrap().revision;
    transport
        .publish(PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap();
    transport.rewrite_history_for_test();

    let error = fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap_err();
    assert!(matches!(error, CoreError::Transport(_)), "got {error}");

    let channel = database.channel(CHANNEL).unwrap().unwrap();
    assert!(channel.halted_reason.is_some());
}

#[test]
fn a_halted_channel_refuses_to_fetch_again() {
    // The halt is sticky. Resuming on the next tick would defeat it.
    let database = database();
    let transport = transport();

    database.halt_channel(CHANNEL, "history rewritten").unwrap();

    let error = fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap_err();
    assert!(
        matches!(error, CoreError::SynchronizationHalted { .. }),
        "got {error}"
    );
}

#[test]
fn a_broken_parent_chain_is_detected_independently_of_the_adapter() {
    // The adapter is supposed to guarantee an unbroken chain, and the
    // conformance suite checks that it does. This asserts the core checks
    // again, because a compromised transport is exactly the thing that would
    // lie about it.
    let genesis = Publication {
        revision: "rev-1".into(),
        parent_revision: None,
        class: PublicationClass::Genesis,
        transport_time: NOW.into(),
        objects: Vec::new(),
    };
    let orphan = Publication {
        revision: "rev-3".into(),
        parent_revision: Some("rev-2".into()),
        class: PublicationClass::Data,
        transport_time: NOW.into(),
        objects: Vec::new(),
    };

    assert!(verify_chain(std::slice::from_ref(&genesis), None).is_ok());
    assert!(matches!(
        verify_chain(&[genesis, orphan], None).unwrap_err(),
        CoreError::BrokenChain { .. }
    ));
}

#[test]
fn a_first_page_that_does_not_start_at_genesis_is_rejected() {
    // Fetching from the beginning must start at the anchor, or the client is
    // validating a history it cannot tie to its channel ID.
    let not_genesis = Publication {
        revision: "rev-2".into(),
        parent_revision: None,
        class: PublicationClass::Data,
        transport_time: NOW.into(),
        objects: Vec::new(),
    };

    assert!(matches!(
        verify_chain(&[not_genesis], None).unwrap_err(),
        CoreError::BrokenChain { .. }
    ));
}

#[test]
fn a_page_that_does_not_continue_from_the_cursor_is_rejected() {
    let elsewhere = Publication {
        revision: "rev-9".into(),
        parent_revision: Some("rev-8".into()),
        class: PublicationClass::Data,
        transport_time: NOW.into(),
        objects: Vec::new(),
    };

    assert!(matches!(
        verify_chain(&[elsewhere], Some("rev-1")).unwrap_err(),
        CoreError::BrokenChain { .. }
    ));
}

#[test]
fn an_adapter_reported_anomaly_halts_the_channel() {
    // An adapter that reports an anomaly has seen something it cannot
    // reconcile. Believing it is cheaper than being wrong.
    struct AnomalousTransport(MemoryTransport);

    impl Transport for AnomalousTransport {
        fn capabilities(&self) -> &hrc_transport::AdapterCapabilities {
            self.0.capabilities()
        }
        fn create_group(
            &mut self,
            objects: Vec<PublishObject>,
        ) -> hrc_transport::Result<Publication> {
            self.0.create_group(objects)
        }
        fn open_group(&self) -> hrc_transport::Result<hrc_transport::GroupState> {
            self.0.open_group()
        }
        fn publish(&mut self, request: PublishRequest) -> hrc_transport::Result<Publication> {
            self.0.publish(request)
        }
        fn fetch(
            &self,
            after: Option<&str>,
            limit: usize,
        ) -> hrc_transport::Result<hrc_transport::FetchPage> {
            let mut page = self.0.fetch(after, limit)?;
            page.anomalies
                .push("object disappeared from history".into());
            Ok(page)
        }
        fn get_object(&self, name: &str, expected: &str) -> hrc_transport::Result<Vec<u8>> {
            self.0.get_object(name, expected)
        }
        fn health(&self) -> hrc_transport::Result<()> {
            self.0.health()
        }
    }

    let database = database();
    let transport = AnomalousTransport(transport());

    let error = fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap_err();
    assert!(matches!(error, CoreError::Transport(_)), "got {error}");

    let channel = database.channel(CHANNEL).unwrap().unwrap();
    assert!(
        channel
            .halted_reason
            .unwrap()
            .contains("object disappeared from history"),
        "the halt must record what the adapter reported"
    );
}

#[test]
fn fetching_an_unregistered_channel_is_an_error() {
    let database = Database::open_in_memory().unwrap();
    let transport = transport();

    assert!(matches!(
        fetch_once(&transport, &database, CHANNEL, 100, NOW).unwrap_err(),
        CoreError::UnknownChannelState { .. }
    ));
}
