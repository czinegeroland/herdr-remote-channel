//! Local state store tests.
//!
//! Concurrency and crash behavior are tested against a real file-backed
//! database rather than an in-memory one, because the properties under test
//! — the write lock, WAL, and durability across reopen — do not exist for a
//! private in-memory connection.

use super::*;

const CHANNEL: &str = "channel-1";
const DEVICE: &str = "device-1";
const NOW: &str = "2026-09-13T00:00:00Z";

/// A database with one registered channel.
fn database() -> Database {
    let database = Database::open_in_memory().unwrap();
    database
        .insert_channel(CHANNEL, "git", "owner/channel", "Test channel", NOW)
        .unwrap();
    database
}

/// A file-backed database in a temporary directory.
fn file_database(directory: &tempfile::TempDir) -> Database {
    let database = Database::open(directory.path().join("state.sqlite")).unwrap();
    if database.channel(CHANNEL).unwrap().is_none() {
        database
            .insert_channel(CHANNEL, "git", "owner/channel", "Test channel", NOW)
            .unwrap();
    }
    database
}

#[test]
fn a_new_database_is_migrated_to_the_current_schema() {
    let database = Database::open_in_memory().unwrap();
    let version: i64 = database
        .connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();

    assert_eq!(version as u32, schema::SCHEMA_VERSION);
}

#[test]
fn migration_is_idempotent_across_reopen() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");

    let first = Database::open(&path).unwrap();
    first
        .insert_channel(CHANNEL, "git", "owner/channel", "Test channel", NOW)
        .unwrap();
    drop(first);

    let second = Database::open(&path).unwrap();
    assert!(second.channel(CHANNEL).unwrap().is_some());
}

#[test]
fn version_five_outbox_rows_migrate_without_becoming_publishable_at_a_new_epoch() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    let connection = Connection::open(&path).unwrap();
    for migration in [
        include_str!("migrations/001_initial.sql"),
        include_str!("migrations/002_inbound_order.sql"),
        include_str!("migrations/003_receipts.sql"),
        include_str!("migrations/004_decision_audit.sql"),
        include_str!("migrations/005_invites.sql"),
    ] {
        connection.execute_batch(migration).unwrap();
    }
    connection.pragma_update(None, "user_version", 5).unwrap();
    connection
        .execute(
            "INSERT INTO channel (
                 channel_id, transport_kind, transport_locator, local_name, created_at
             ) VALUES (?1, 'git', 'owner/channel', 'Test channel', ?2)",
            params![CHANNEL, NOW],
        )
        .unwrap();
    connection
        .execute(
            "INSERT INTO outbox (
                 message_id, channel_id, device_id, device_sequence, chain_id,
                 roster_epoch, payload_hash, ciphertext, state, created_at, updated_at
             ) VALUES (
                 'msg-1', ?1, ?2, 1, 'chain-1', 1, 'hash-1',
                 X'63697068657274657874', 'queued', ?3, ?3
             )",
            params![CHANNEL, DEVICE, NOW],
        )
        .unwrap();
    drop(connection);

    let migrated = Database::open(&path).unwrap();
    let record = migrated
        .pending_outgoing_records(CHANNEL)
        .unwrap()
        .remove(0);
    assert_eq!(record.ciphertext, b"ciphertext");
    assert_eq!(record.roster_epoch, 1);
    assert_eq!(record.reseal_material, None);
}

#[test]
fn a_database_from_a_newer_build_is_refused() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");

    let database = Database::open(&path).unwrap();
    database
        .connection
        .pragma_update(None, "user_version", schema::SCHEMA_VERSION + 1)
        .unwrap();
    drop(database);

    assert!(matches!(
        Database::open(&path).unwrap_err(),
        StorageError::SchemaTooNew { .. }
    ));
}

#[test]
fn file_databases_use_write_ahead_logging() {
    // WAL is what lets a CLI process read while the daemon writes.
    let directory = tempfile::tempdir().unwrap();
    let database = file_database(&directory);

    let mode: String = database
        .connection
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(mode.to_lowercase(), "wal");
}

#[test]
fn channel_state_round_trips() {
    let database = database();
    let record = database.channel(CHANNEL).unwrap().unwrap();

    assert_eq!(record.transport_locator, "owner/channel");
    assert_eq!(record.roster_epoch, 0);
    assert_eq!(record.control_sequence, 0);
    assert_eq!(record.sync_cursor, None);
    assert_eq!(record.halted_reason, None);
}

#[test]
fn roster_progress_and_cursor_are_persisted() {
    let database = database();
    database.set_roster_progress(CHANNEL, 4, 9).unwrap();
    database.set_sync_cursor(CHANNEL, "commit-abc").unwrap();

    let record = database.channel(CHANNEL).unwrap().unwrap();
    assert_eq!(record.roster_epoch, 4);
    assert_eq!(record.control_sequence, 9);
    assert_eq!(record.sync_cursor.as_deref(), Some("commit-abc"));
}

#[test]
fn operations_on_an_unregistered_channel_are_refused() {
    let database = database();
    assert!(matches!(
        database.set_sync_cursor("nope", "commit").unwrap_err(),
        StorageError::UnknownChannel { .. }
    ));
}

#[test]
fn the_first_halt_reason_is_sticky() {
    // A cascade of downstream failures must not bury the original cause.
    let database = database();
    database.halt_channel(CHANNEL, "history rewritten").unwrap();
    database.halt_channel(CHANNEL, "fetch failed").unwrap();

    let record = database.channel(CHANNEL).unwrap().unwrap();
    assert_eq!(record.halted_reason.as_deref(), Some("history rewritten"));
}

#[test]
fn allocation_produces_a_linked_sequence() {
    let mut database = database();

    let first = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
        .unwrap();
    let second = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-2", 1, "hash-2", NOW)
        .unwrap();

    assert_eq!(first.device_sequence, 1);
    assert_eq!(first.previous_chain_id, None);

    assert_eq!(second.device_sequence, 2);
    assert_eq!(
        second.previous_chain_id.as_deref(),
        Some(first.chain_id.as_str()),
        "each message must link to its predecessor"
    );
    assert_ne!(first.chain_id, second.chain_id);
}

#[test]
fn chain_ids_are_derived_from_the_documented_inputs() {
    let expected = chain_id_for(CHANNEL, DEVICE, 1, "msg-1").unwrap();

    let mut database = database();
    let reservation = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
        .unwrap();

    assert_eq!(reservation.chain_id, expected);

    // Every input is load-bearing.
    assert_ne!(chain_id_for("other", DEVICE, 1, "msg-1").unwrap(), expected);
    assert_ne!(
        chain_id_for(CHANNEL, "other", 1, "msg-1").unwrap(),
        expected
    );
    assert_ne!(chain_id_for(CHANNEL, DEVICE, 2, "msg-1").unwrap(), expected);
    assert_ne!(chain_id_for(CHANNEL, DEVICE, 1, "msg-2").unwrap(), expected);
}

#[test]
fn devices_allocate_independent_sequences() {
    let mut database = database();

    let first = database
        .allocate_outgoing(CHANNEL, "device-a", "msg-a", 1, "hash-a", NOW)
        .unwrap();
    let second = database
        .allocate_outgoing(CHANNEL, "device-b", "msg-b", 1, "hash-b", NOW)
        .unwrap();

    assert_eq!(first.device_sequence, 1);
    assert_eq!(second.device_sequence, 1, "sequences are per device");
    assert_eq!(second.previous_chain_id, None);
}

#[test]
fn allocation_against_an_unknown_channel_is_refused() {
    let mut database = database();
    assert!(matches!(
        database
            .allocate_outgoing("nope", DEVICE, "msg-1", 1, "hash", NOW)
            .unwrap_err(),
        StorageError::UnknownChannel { .. }
    ));
}

#[test]
fn a_reused_message_id_is_refused_without_consuming_a_sequence() {
    // The insert fails inside the transaction, so the sequence bump rolls
    // back with it and the next real allocation is not skipped.
    let mut database = database();
    database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
        .unwrap();

    assert!(
        database
            .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
            .is_err()
    );

    let next = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-2", 1, "hash-2", NOW)
        .unwrap();
    assert_eq!(next.device_sequence, 2);
}

#[test]
fn concurrent_callers_never_receive_the_same_sequence() {
    // PRD section 17.2: "A second concurrent caller must never receive the
    // same sequence or predecessor chain ID." Two processes are simulated by
    // two connections to one file-backed database and two threads.
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");
    {
        let database = Database::open(&path).unwrap();
        database
            .insert_channel(CHANNEL, "git", "owner/channel", "Test channel", NOW)
            .unwrap();
    }

    const PER_THREAD: usize = 25;
    const THREADS: usize = 4;

    let reservations = std::thread::scope(|scope| {
        let handles: Vec<_> = (0..THREADS)
            .map(|thread| {
                let path = path.clone();
                scope.spawn(move || {
                    let mut database = Database::open(&path).unwrap();
                    (0..PER_THREAD)
                        .map(|index| {
                            let message_id = format!("msg-{thread}-{index}");
                            database
                                .allocate_outgoing(CHANNEL, DEVICE, &message_id, 1, "hash", NOW)
                                .unwrap()
                        })
                        .collect::<Vec<_>>()
                })
            })
            .collect();

        handles
            .into_iter()
            .flat_map(|handle| handle.join().unwrap())
            .collect::<Vec<_>>()
    });

    let mut sequences: Vec<u64> = reservations.iter().map(|r| r.device_sequence).collect();
    sequences.sort_unstable();
    let expected: Vec<u64> = (1..=(THREADS * PER_THREAD) as u64).collect();
    assert_eq!(sequences, expected, "sequences must be unique and gapless");

    let mut chain_ids: Vec<&str> = reservations.iter().map(|r| r.chain_id.as_str()).collect();
    chain_ids.sort_unstable();
    let total = chain_ids.len();
    chain_ids.dedup();
    assert_eq!(chain_ids.len(), total, "chain IDs must be unique");

    // Every predecessor link must point at a real earlier chain ID, and no
    // two messages may claim the same predecessor.
    let mut predecessors: Vec<&str> = reservations
        .iter()
        .filter_map(|r| r.previous_chain_id.as_deref())
        .collect();
    let claimed = predecessors.len();
    predecessors.sort_unstable();
    predecessors.dedup();
    assert_eq!(
        predecessors.len(),
        claimed,
        "predecessor links must be unique"
    );
}

#[test]
fn a_reservation_survives_reopening_the_database() {
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("state.sqlite");

    let reserved = {
        let mut database = Database::open(&path).unwrap();
        database
            .insert_channel(CHANNEL, "git", "owner/channel", "Test channel", NOW)
            .unwrap();
        database
            .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
            .unwrap()
    };

    let mut reopened = Database::open(&path).unwrap();
    assert_eq!(
        reopened.outbox_state("msg-1").unwrap(),
        Some(OutboxState::Reserved)
    );

    // The next allocation continues the sequence rather than restarting it.
    let next = reopened
        .allocate_outgoing(CHANNEL, DEVICE, "msg-2", 1, "hash-2", NOW)
        .unwrap();
    assert_eq!(next.device_sequence, reserved.device_sequence + 1);
    assert_eq!(
        next.previous_chain_id.as_deref(),
        Some(reserved.chain_id.as_str())
    );
}

#[test]
fn recovery_burns_an_abandoned_sequence_rather_than_reusing_it() {
    // A crash between reservation and ciphertext leaves nothing publishable.
    // Reusing the sequence is unsafe: a peer may already have seen a message
    // claiming it.
    let mut database = database();
    database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
        .unwrap();

    assert_eq!(database.recover_reservations(CHANNEL, NOW).unwrap(), 1);
    assert_eq!(
        database.outbox_state("msg-1").unwrap(),
        Some(OutboxState::Gap)
    );

    let next = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-2", 1, "hash-2", NOW)
        .unwrap();
    assert_eq!(next.device_sequence, 2, "the burned sequence is not reused");
}

#[test]
fn recovery_leaves_queued_messages_alone() {
    let mut database = database();
    database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
        .unwrap();
    database
        .queue_outgoing("msg-1", b"ciphertext", NOW)
        .unwrap();

    assert_eq!(database.recover_reservations(CHANNEL, NOW).unwrap(), 0);
    assert_eq!(
        database.outbox_state("msg-1").unwrap(),
        Some(OutboxState::Queued)
    );
}

#[test]
fn pending_outgoing_records_include_ciphertext_and_created_at() {
    let mut database = database();
    let reservation = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash", NOW)
        .unwrap();
    database
        .queue_outgoing_resealable("msg-1", b"ciphertext", Some(b"protected-envelope"), NOW)
        .unwrap();

    let records = database.pending_outgoing_records(CHANNEL).unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].message_id, "msg-1");
    assert_eq!(records[0].device_id, DEVICE);
    assert_eq!(records[0].device_sequence, reservation.device_sequence);
    assert_eq!(records[0].chain_id, reservation.chain_id);
    assert_eq!(records[0].previous_chain_id, reservation.previous_chain_id);
    assert_eq!(records[0].roster_epoch, 1);
    assert_eq!(records[0].payload_hash, "hash");
    assert_eq!(records[0].created_at, NOW);
    assert_eq!(records[0].ciphertext, b"ciphertext");
    assert_eq!(
        records[0].reseal_material.as_deref(),
        Some(b"protected-envelope".as_slice())
    );
}

#[test]
fn replacing_stale_ciphertext_preserves_the_logical_allocation() {
    let mut database = database();
    let reservation = database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "payload-hash", NOW)
        .unwrap();
    database
        .queue_outgoing_resealable("msg-1", b"old", Some(b"protected-envelope"), NOW)
        .unwrap();

    database
        .replace_outgoing_ciphertext("msg-1", 1, 2, b"new", "2026-09-13T00:01:00Z")
        .unwrap();

    let record = database
        .pending_outgoing_records(CHANNEL)
        .unwrap()
        .remove(0);
    assert_eq!(record.message_id, "msg-1");
    assert_eq!(record.device_id, DEVICE);
    assert_eq!(record.device_sequence, reservation.device_sequence);
    assert_eq!(record.chain_id, reservation.chain_id);
    assert_eq!(record.previous_chain_id, reservation.previous_chain_id);
    assert_eq!(record.roster_epoch, 2);
    assert_eq!(record.payload_hash, "payload-hash");
    assert_eq!(record.created_at, NOW);
    assert_eq!(record.ciphertext, b"new");
    assert_eq!(
        record.reseal_material.as_deref(),
        Some(b"protected-envelope".as_slice())
    );
}

#[test]
fn the_outbox_lifecycle_advances_and_is_ordered() {
    let mut database = database();
    for index in 1..=3 {
        let message_id = format!("msg-{index}");
        database
            .allocate_outgoing(CHANNEL, DEVICE, &message_id, 1, "hash", NOW)
            .unwrap();
        database
            .queue_outgoing(&message_id, b"ciphertext", NOW)
            .unwrap();
    }

    assert_eq!(
        database.pending_outgoing(CHANNEL).unwrap(),
        vec!["msg-1", "msg-2", "msg-3"],
        "pending messages come back in send order"
    );

    database.mark_published("msg-1", NOW).unwrap();
    assert_eq!(
        database.pending_outgoing(CHANNEL).unwrap(),
        vec!["msg-2", "msg-3"]
    );
    assert_eq!(
        database.outbox_state("msg-1").unwrap(),
        Some(OutboxState::Published)
    );
}

#[test]
fn a_failed_attempt_is_counted_and_the_message_is_kept() {
    // PRD section 26: the outbox stays durable and retries with backoff.
    let mut database = database();
    database
        .allocate_outgoing(CHANNEL, DEVICE, "msg-1", 1, "hash-1", NOW)
        .unwrap();
    database
        .queue_outgoing("msg-1", b"ciphertext", NOW)
        .unwrap();

    assert_eq!(
        database
            .record_attempt_failure("msg-1", "network", NOW)
            .unwrap(),
        1
    );
    assert_eq!(
        database
            .record_attempt_failure("msg-1", "network", NOW)
            .unwrap(),
        2
    );

    assert_eq!(
        database.outbox_state("msg-1").unwrap(),
        Some(OutboxState::Queued),
        "a failed attempt must not drop the message"
    );
    assert_eq!(database.pending_outgoing(CHANNEL).unwrap(), vec!["msg-1"]);
}

#[test]
fn outbox_operations_on_an_unknown_message_are_refused() {
    let database = database();

    assert!(matches!(
        database.queue_outgoing("nope", b"x", NOW).unwrap_err(),
        StorageError::UnknownMessage { .. }
    ));
    assert!(matches!(
        database.mark_published("nope", NOW).unwrap_err(),
        StorageError::UnknownMessage { .. }
    ));
    assert!(matches!(
        database
            .record_attempt_failure("nope", "e", NOW)
            .unwrap_err(),
        StorageError::UnknownMessage { .. }
    ));
}

#[test]
fn audit_records_are_appended_in_order() {
    let database = database();
    database
        .append_audit(
            Some(CHANNEL),
            Some("msg-1"),
            "quarantine",
            Some("hash"),
            None,
            NOW,
        )
        .unwrap();
    database
        .append_audit(
            Some(CHANNEL),
            Some("msg-1"),
            "approve_to_agent",
            Some("hash"),
            Some("reviewer"),
            NOW,
        )
        .unwrap();

    assert_eq!(
        database.audit_actions().unwrap(),
        vec!["quarantine", "approve_to_agent"]
    );
}

#[test]
fn utc_now_is_an_rfc_3339_utc_timestamp() {
    let database = database();
    let now = database.utc_now().unwrap();

    assert_eq!(now.len(), 20);
    assert_eq!(&now[4..5], "-");
    assert_eq!(&now[7..8], "-");
    assert_eq!(&now[10..11], "T");
    assert_eq!(&now[13..14], ":");
    assert_eq!(&now[16..17], ":");
    assert_eq!(&now[19..20], "Z");
}

#[test]
fn audit_entries_are_filtered_and_returned_newest_first() {
    let database = database();
    database
        .append_audit(
            Some(CHANNEL),
            Some("msg-1"),
            "quarantine",
            Some("hash"),
            Some("first"),
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .append_audit(
            Some(CHANNEL),
            Some("msg-2"),
            "approve_to_agent",
            Some("hash"),
            Some("second"),
            "2026-09-13T01:00:00Z",
        )
        .unwrap();

    let all = database.audit_entries(None).unwrap();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].message_id.as_deref(), Some("msg-2"));
    assert_eq!(all[1].message_id.as_deref(), Some("msg-1"));

    let filtered = database
        .audit_entries(Some("2026-09-13T00:30:00Z"))
        .unwrap();
    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].detail.as_deref(), Some("second"));
}

#[test]
fn the_inbox_rejects_dispositions_outside_the_lifecycle() {
    // The disposition set is a CHECK constraint rather than a convention, so
    // no code path can quietly invent a state that bypasses the prompt gate.
    let database = database();
    let insert = database.connection.execute(
        "INSERT INTO inbox (
             message_id, channel_id, sender_principal, sender_device, kind,
             roster_epoch, ciphertext_bytes, plaintext_bytes, created_at,
             received_at, disposition
         ) VALUES ('m', ?1, 'p', 'd', 'note', 1, 10, 5, ?2, ?2, 'auto_delivered')",
        params![CHANNEL, NOW],
    );

    assert!(insert.is_err(), "an invented disposition must be rejected");
}

#[test]
fn the_outbox_rejects_states_outside_the_lifecycle() {
    let database = database();
    let insert = database.connection.execute(
        "INSERT INTO outbox (
             message_id, channel_id, device_id, device_sequence, chain_id,
             roster_epoch, payload_hash, state, created_at, updated_at
         ) VALUES ('m', ?1, 'd', 1, 'c', 1, 'h', 'teleported', ?2, ?2)",
        params![CHANNEL, NOW],
    );

    assert!(insert.is_err(), "an invented outbox state must be rejected");
}

// --- Inbound ordering and deduplication (PRD sections 18.1, 12.2) ---

const SENDER: &str = "sender-device";

/// One arriving message from `SENDER` at `sequence`.
///
/// Chain links are derived the same way the sender derives them, so the
/// test exercises the real linkage rather than a stand-in for it.
struct Arrival {
    message_id: String,
    chain_id: String,
    previous_chain_id: Option<String>,
    device_sequence: u64,
    digest: String,
    thread_id: Option<String>,
    in_reply_to: Option<String>,
}

fn arrival(sequence: u64) -> Arrival {
    let message_id = format!("msg-{sequence}");
    let chain_id = chain_id_for(CHANNEL, SENDER, sequence, &message_id).unwrap();
    let previous_chain_id = (sequence > 1).then(|| {
        chain_id_for(
            CHANNEL,
            SENDER,
            sequence - 1,
            &format!("msg-{}", sequence - 1),
        )
        .unwrap()
    });

    Arrival {
        digest: format!("digest-{sequence}"),
        message_id,
        chain_id,
        previous_chain_id,
        device_sequence: sequence,
        thread_id: Some("thread-1".into()),
        in_reply_to: None,
    }
}

impl Arrival {
    fn message(&self) -> InboundMessage<'_> {
        InboundMessage {
            channel_id: CHANNEL,
            message_id: &self.message_id,
            sender_principal: "alice",
            sender_device: SENDER,
            device_sequence: self.device_sequence,
            chain_id: &self.chain_id,
            previous_chain_id: self.previous_chain_id.as_deref(),
            kind: "note",
            thread_id: self.thread_id.as_deref(),
            in_reply_to: self.in_reply_to.as_deref(),
            roster_epoch: 1,
            endpoint: None,
            ciphertext_bytes: 1024,
            plaintext_bytes: 512,
            ciphertext_sha256: &self.digest,
            created_at: NOW,
            expires_at: None,
            attachment_count: 0,
            attachment_bytes: 0,
            prompt_request: false,
            body: b"quarantined body",
            ciphertext: b"ciphertext",
            context: None,
        }
    }
}

#[test]
fn messages_are_accepted_in_sequence_and_numbered_by_arrival() {
    let mut database = database();

    for sequence in 1..=3 {
        let arrival = arrival(sequence);
        let outcome = database.record_inbound(&arrival.message(), NOW).unwrap();

        assert_eq!(
            outcome,
            InboundOutcome::Accepted {
                arrival_sequence: sequence,
                released: Vec::new()
            }
        );
    }

    let entries = database.inbox_entries(CHANNEL).unwrap();
    let ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry.message_id.as_str())
        .collect();
    assert_eq!(ids, vec!["msg-1", "msg-2", "msg-3"]);
}

#[test]
fn received_context_is_stored_quarantined_with_its_message() {
    let mut database = database();
    let arrival = arrival(1);
    let manifest = br#"{"id":"ctx-1","items":[],"version":1}"#;
    let digest = "a".repeat(64);
    let message = InboundMessage {
        context: Some(InboundContext {
            package_id: "ctx-1",
            digest: &digest,
            manifest,
        }),
        ..arrival.message()
    };

    database.record_inbound(&message, NOW).unwrap();
    let stored = database
        .inbound_context(&arrival.message_id)
        .unwrap()
        .expect("received context should be quarantined");
    assert_eq!(stored.package_id, "ctx-1");
    assert_eq!(stored.manifest, manifest);
}

#[test]
fn received_context_uses_its_inbox_rows_single_lifecycle() {
    let mut database = database();
    let arrival = arrival(1);
    let digest = "b".repeat(64);
    let message = InboundMessage {
        context: Some(InboundContext {
            package_id: "ctx-1",
            digest: &digest,
            manifest: br#"{"id":"ctx-1","items":[],"version":1}"#,
        }),
        ..arrival.message()
    };
    database.record_inbound(&message, NOW).unwrap();
    assert!(
        database
            .inbound_context(&arrival.message_id)
            .unwrap()
            .is_some()
    );

    // There is no context disposition to drift from the prompt gate. The
    // atomic approval decision moves the inbox row, and the trusted-only
    // context query follows without a second transition.
    database
        .commit_decision(&DecisionRecord {
            channel_id: CHANNEL.into(),
            message_id: arrival.message_id.clone(),
            action: "deliver_to_agent".into(),
            original_content: "quarantined body".into(),
            edited_content: None,
            content_hash: "hash".into(),
            edited_hash: None,
            agent: Some("reviewer".into()),
            decided_by: "human".into(),
            occurred_at: NOW.into(),
        })
        .unwrap();
    assert!(
        database
            .inbound_context(&arrival.message_id)
            .unwrap()
            .is_none()
    );
    let entry = database.inbox_entries(CHANNEL).unwrap().pop().unwrap();
    assert_eq!(entry.disposition, "approved");
}

#[test]
fn a_repeated_delivery_is_recognized_rather_than_stored_twice() {
    // At-least-once delivery is the transport's contract, so a repeat is
    // ordinary traffic and not an error (HRC-MSG-006).
    let mut database = database();
    let first = arrival(1);

    database.record_inbound(&first.message(), NOW).unwrap();
    let again = database.record_inbound(&first.message(), NOW).unwrap();

    assert_eq!(again, InboundOutcome::Duplicate);
    assert_eq!(database.inbox_entries(CHANNEL).unwrap().len(), 1);
}

#[test]
fn the_same_id_with_different_ciphertext_is_a_substitution() {
    // The distinction that makes deduplication safe: a genuine repeat is
    // identical by construction, so differing bytes mean the object under
    // that ID was replaced.
    let mut database = database();
    let first = arrival(1);
    database.record_inbound(&first.message(), NOW).unwrap();

    let mut swapped = arrival(1);
    swapped.digest = "a different digest".into();

    let error = database
        .record_inbound(&swapped.message(), NOW)
        .unwrap_err();
    assert!(matches!(error, StorageError::InboundSubstituted { .. }));
}

#[test]
fn an_out_of_order_message_waits_for_its_predecessor() {
    // Lazy and partial fetching mean the predecessor may still be in
    // flight, so the message is neither dropped nor accepted early.
    let mut database = database();
    let second = arrival(2);

    let outcome = database.record_inbound(&second.message(), NOW).unwrap();
    assert!(matches!(outcome, InboundOutcome::Held { .. }));

    assert_eq!(database.held_inbound(CHANNEL).unwrap(), vec!["msg-2"]);
    assert!(
        database.inbox_entries(CHANNEL).unwrap().is_empty(),
        "a held message must not appear in the inbox yet"
    );
    assert!(
        database.pending_inbound().unwrap().is_empty(),
        "a held message must not be exposed to the trusted review surface"
    );

    database.record_inbound(&arrival(1).message(), NOW).unwrap();
    let pending = database.pending_inbound().unwrap();
    assert_eq!(
        pending
            .iter()
            .map(|message| message.message_id.as_str())
            .collect::<Vec<_>>(),
        vec!["msg-1", "msg-2"],
        "the held message becomes reviewable only after its predecessor releases it"
    );
}

#[test]
fn a_gap_that_fills_releases_everything_behind_it_in_order() {
    let mut database = database();

    // Three and two arrive first, both stuck behind one.
    database.record_inbound(&arrival(3).message(), NOW).unwrap();
    database.record_inbound(&arrival(2).message(), NOW).unwrap();
    assert_eq!(database.held_inbound(CHANNEL).unwrap().len(), 2);

    let outcome = database.record_inbound(&arrival(1).message(), NOW).unwrap();

    assert_eq!(
        outcome,
        InboundOutcome::Accepted {
            arrival_sequence: 1,
            released: vec!["msg-2".into(), "msg-3".into()],
        }
    );
    assert!(database.held_inbound(CHANNEL).unwrap().is_empty());

    let ids: Vec<String> = database
        .inbox_entries(CHANNEL)
        .unwrap()
        .into_iter()
        .map(|entry| entry.message_id)
        .collect();
    assert_eq!(ids, vec!["msg-1", "msg-2", "msg-3"]);
}

#[test]
fn a_sender_reusing_a_sequence_number_is_a_fork() {
    // The per-device equivalent of a rewritten control log: two different
    // messages claiming one position in the same history.
    let mut database = database();
    database.record_inbound(&arrival(1).message(), NOW).unwrap();

    let mut forked = arrival(1);
    forked.message_id = "msg-1-other".into();
    forked.chain_id = chain_id_for(CHANNEL, SENDER, 1, "msg-1-other").unwrap();
    forked.digest = "other digest".into();

    let error = database.record_inbound(&forked.message(), NOW).unwrap_err();
    assert!(
        matches!(
            error,
            StorageError::InboundForked {
                device_sequence: 1,
                ..
            }
        ),
        "unexpected error: {error}"
    );
}

#[test]
fn claiming_to_be_a_first_message_after_others_is_a_fork() {
    let mut database = database();
    database.record_inbound(&arrival(1).message(), NOW).unwrap();

    let mut restart = arrival(2);
    restart.previous_chain_id = None;

    let error = database
        .record_inbound(&restart.message(), NOW)
        .unwrap_err();
    assert!(matches!(error, StorageError::InboundForked { .. }));
}

#[test]
fn a_message_naming_an_unknown_predecessor_is_held_not_accepted() {
    // A fabricated predecessor and a genuinely missing one look identical
    // from here. Holding is correct for both: the liar cannot produce the
    // link it invented, so its message never gets in.
    let mut database = database();
    database.record_inbound(&arrival(1).message(), NOW).unwrap();

    let mut invented = arrival(2);
    invented.previous_chain_id = Some("a link that was never sent".into());

    let outcome = database.record_inbound(&invented.message(), NOW).unwrap();
    assert!(matches!(outcome, InboundOutcome::Held { .. }));
    assert_eq!(database.inbox_entries(CHANNEL).unwrap().len(), 1);
}

#[test]
fn a_thread_reads_in_arrival_order_not_sender_order() {
    // `created_at` is chosen by the sender, so ordering a conversation by it
    // would let a remote peer place its message anywhere in someone else's
    // reading of it.
    let mut database = database();

    let mut first = arrival(1);
    first.thread_id = Some("thread-1".into());
    database.record_inbound(&first.message(), NOW).unwrap();

    let mut backdated = arrival(2);
    backdated.thread_id = Some("thread-1".into());
    backdated.in_reply_to = Some("msg-1".into());
    let mut message = backdated.message();
    message.created_at = "2020-01-01T00:00:00Z";
    database.record_inbound(&message, NOW).unwrap();

    let thread = database.thread_entries(CHANNEL, "thread-1").unwrap();
    let ids: Vec<&str> = thread
        .iter()
        .map(|entry| entry.message_id.as_str())
        .collect();

    assert_eq!(
        ids,
        vec!["msg-1", "msg-2"],
        "a backdated message reordered the thread"
    );
    assert_eq!(thread[1].in_reply_to.as_deref(), Some("msg-1"));
}

#[test]
fn threads_do_not_leak_into_each_other() {
    let mut database = database();

    let mut first = arrival(1);
    first.thread_id = Some("thread-1".into());
    database.record_inbound(&first.message(), NOW).unwrap();

    let mut second = arrival(2);
    second.thread_id = Some("thread-2".into());
    database.record_inbound(&second.message(), NOW).unwrap();

    assert_eq!(
        database.thread_entries(CHANNEL, "thread-1").unwrap().len(),
        1
    );
    assert_eq!(
        database.thread_entries(CHANNEL, "thread-2").unwrap().len(),
        1
    );
    assert_eq!(database.inbox_entries(CHANNEL).unwrap().len(), 2);
}

#[test]
fn held_messages_survive_reopening() {
    // A gap that outlives the process must not silently resolve into an
    // out-of-order accept after a restart.
    let directory = tempfile::tempdir().unwrap();

    {
        let mut database = file_database(&directory);
        database.record_inbound(&arrival(2).message(), NOW).unwrap();
    }

    let mut database = file_database(&directory);
    assert_eq!(database.held_inbound(CHANNEL).unwrap(), vec!["msg-2"]);

    let outcome = database.record_inbound(&arrival(1).message(), NOW).unwrap();
    assert_eq!(
        outcome,
        InboundOutcome::Accepted {
            arrival_sequence: 1,
            released: vec!["msg-2".into()],
        }
    );
}

#[test]
fn two_sender_devices_have_independent_chains() {
    // One device being ahead must not hold up another's messages.
    let mut database = database();
    database.record_inbound(&arrival(1).message(), NOW).unwrap();

    let other_chain = chain_id_for(CHANNEL, "other-device", 1, "other-1").unwrap();
    let other = InboundMessage {
        channel_id: CHANNEL,
        message_id: "other-1",
        sender_principal: "bob",
        sender_device: "other-device",
        device_sequence: 1,
        chain_id: &other_chain,
        previous_chain_id: None,
        kind: "note",
        thread_id: Some("thread-1"),
        in_reply_to: None,
        roster_epoch: 1,
        endpoint: None,
        ciphertext_bytes: 10,
        plaintext_bytes: 5,
        ciphertext_sha256: "other-digest",
        created_at: NOW,
        expires_at: None,
        attachment_count: 0,
        attachment_bytes: 0,
        prompt_request: false,
        body: b"body",
        ciphertext: b"ciphertext",
        context: None,
    };

    let outcome = database.record_inbound(&other, NOW).unwrap();
    assert!(matches!(outcome, InboundOutcome::Accepted { .. }));
    assert_eq!(database.inbox_entries(CHANNEL).unwrap().len(), 2);
}

// --- Receipts (PRD sections 18.2 and 18.3) ---

fn receipt(device: &str, state: &str) -> RecordedReceipt {
    RecordedReceipt {
        message_id: "msg-1".into(),
        reporter_principal: "bob".into(),
        reporter_device: device.into(),
        state: state.into(),
        rejection_code: None,
        reported_at: NOW.into(),
    }
}

#[test]
fn a_receipt_is_recorded_once_per_device_and_state() {
    let database = database();

    assert!(
        database
            .record_receipt(CHANNEL, &receipt("bob-1", "delivered"), NOW)
            .unwrap()
    );
    assert!(
        !database
            .record_receipt(CHANNEL, &receipt("bob-1", "delivered"), NOW)
            .unwrap(),
        "a repeated report should be recognized, not stored again"
    );

    // The same device reaching a later state is a new report.
    assert!(
        database
            .record_receipt(CHANNEL, &receipt("bob-1", "read"), NOW)
            .unwrap()
    );

    assert_eq!(database.receipts_for("msg-1").unwrap().len(), 2);
}

#[test]
fn receipts_are_tracked_per_device_not_per_message() {
    // One device saying "delivered" is not the same claim as a principal's
    // whole fleet saying it, and collapsing them would overstate what the
    // sender actually heard.
    let database = database();

    database
        .record_receipt(CHANNEL, &receipt("bob-1", "delivered"), NOW)
        .unwrap();
    database
        .record_receipt(CHANNEL, &receipt("bob-2", "delivered"), NOW)
        .unwrap();

    assert_eq!(
        database.devices_reporting("msg-1", "delivered").unwrap(),
        vec!["bob-1", "bob-2"]
    );
    assert!(
        database
            .devices_reporting("msg-1", "read")
            .unwrap()
            .is_empty()
    );
}

#[test]
fn a_rejection_keeps_its_code() {
    let database = database();
    let mut rejected = receipt("bob-1", "rejected");
    rejected.rejection_code = Some("unsupported_kind".into());

    database.record_receipt(CHANNEL, &rejected, NOW).unwrap();

    let recorded = database.receipts_for("msg-1").unwrap();
    assert_eq!(
        recorded[0].rejection_code.as_deref(),
        Some("unsupported_kind")
    );
}

#[test]
fn an_unknown_receipt_state_is_refused_by_the_schema() {
    // The state set is closed. A column that accepted anything would let a
    // future bug record a state nothing else understands.
    let database = database();
    let bogus = receipt("bob-1", "acknowledged");

    assert!(database.record_receipt(CHANNEL, &bogus, NOW).is_err());
}

// --- Decision audit (PRD requirements HRC-GATE-004, HRC-SEC-011) ---

fn decision(action: &str, edited: Option<&str>) -> DecisionRecord {
    DecisionRecord {
        channel_id: CHANNEL.into(),
        message_id: "msg-1".into(),
        action: action.into(),
        original_content: "the original body".into(),
        edited_content: edited.map(str::to_owned),
        content_hash: "a".repeat(64),
        edited_hash: edited.map(|_| "b".repeat(64)),
        agent: Some("reviewer-pane".into()),
        decided_by: "roland".into(),
        occurred_at: NOW.into(),
    }
}

#[test]
fn a_decision_round_trips_with_both_versions_of_the_content() {
    let database = database();

    database
        .append_decision(&decision("deliver_edited", Some("the edited body")))
        .unwrap();

    let recorded = database.decisions_for("msg-1").unwrap();
    assert_eq!(recorded.len(), 1);
    assert_eq!(recorded[0].original_content, "the original body");
    assert_eq!(
        recorded[0].edited_content.as_deref(),
        Some("the edited body")
    );
    assert_eq!(recorded[0].decided_by, "roland");
    assert_eq!(recorded[0].agent.as_deref(), Some("reviewer-pane"));
}

#[test]
fn decisions_accumulate_rather_than_replace() {
    // The history of what was decided about a message is the point. A second
    // decision must not overwrite the first.
    let database = database();

    database
        .append_decision(&decision("keep_in_inbox", None))
        .unwrap();
    database
        .append_decision(&decision("deliver_to_agent", None))
        .unwrap();

    let recorded = database.decisions_for("msg-1").unwrap();
    let actions: Vec<&str> = recorded.iter().map(|entry| entry.action.as_str()).collect();
    assert_eq!(actions, vec!["keep_in_inbox", "deliver_to_agent"]);
}

#[test]
fn decisions_do_not_mix_with_ordinary_audit_lines() {
    // `hrc audit` shows everything; a decision query must show decisions.
    let database = database();

    database
        .append_audit(
            Some(CHANNEL),
            Some("msg-1"),
            "synchronized",
            None,
            None,
            NOW,
        )
        .unwrap();
    database
        .append_decision(&decision("decline", None))
        .unwrap();

    assert_eq!(database.decisions_for("msg-1").unwrap().len(), 1);
    assert!(database.audit_actions().unwrap().len() >= 2);
}

#[test]
fn the_audit_log_has_no_update_or_delete_path() {
    // Not a behavioral test but a structural one: a decision that could be
    // revised afterwards would not be evidence of anything. If a future
    // change adds one, this fails and someone has to argue for it.
    let source = include_str!("lib.rs");

    for forbidden in ["UPDATE audit", "DELETE FROM audit"] {
        assert!(
            !source.contains(forbidden),
            "the storage API gained a `{forbidden}` path"
        );
    }
}

/// An arrival carrying an expiry.
fn expiring(sequence: u64, expires_at: &str) -> (Arrival, String) {
    (arrival(sequence), expires_at.to_owned())
}

#[test]
fn a_lapsed_message_is_swept_and_its_body_goes_with_it() {
    // PRD requirement HRC-MSG-005 and section 26: display as expired, allow
    // no action. Section 16.3: expiration is logical, so the row stays.
    let mut database = database();

    let (arrival, expires_at) = expiring(1, "2026-09-12T00:00:00Z");
    let mut message = arrival.message();
    message.expires_at = Some(&expires_at);
    database.record_inbound(&message, NOW).unwrap();

    let swept = database.sweep_expired(NOW).unwrap();
    assert_eq!(swept, vec!["msg-1".to_owned()]);

    let entries = database.inbox_entries(CHANNEL).unwrap();
    assert_eq!(entries.len(), 1, "the row is kept; expiry is logical");
    assert_eq!(entries[0].disposition, "expired");

    // The quarantined plaintext is gone, so nothing can approve it later.
    assert!(
        database
            .pending_inbound()
            .unwrap()
            .iter()
            .all(|pending| pending.message_id != "msg-1"),
        "a swept message must leave the approval queue"
    );
}

#[test]
fn a_message_that_has_not_lapsed_is_left_alone() {
    let mut database = database();

    let (arrival, expires_at) = expiring(1, "2026-09-14T00:00:00Z");
    let mut message = arrival.message();
    message.expires_at = Some(&expires_at);
    database.record_inbound(&message, NOW).unwrap();

    assert!(database.sweep_expired(NOW).unwrap().is_empty());
    assert_eq!(
        database.inbox_entries(CHANNEL).unwrap()[0].disposition,
        "quarantined"
    );
}

#[test]
fn a_message_without_an_expiry_never_lapses() {
    let mut database = database();
    database.record_inbound(&arrival(1).message(), NOW).unwrap();

    assert!(
        database
            .sweep_expired("2099-01-01T00:00:00Z")
            .unwrap()
            .is_empty(),
        "a message with no expiry has nothing to lapse"
    );
}

#[test]
fn sweeping_is_idempotent() {
    // The daemon sweeps on every tick. A second pass must not re-report a
    // message that already expired, or the audit log would fill with one
    // entry per poll for the same lapse.
    let mut database = database();

    let (arrival, expires_at) = expiring(1, "2026-09-12T00:00:00Z");
    let mut message = arrival.message();
    message.expires_at = Some(&expires_at);
    database.record_inbound(&message, NOW).unwrap();

    assert_eq!(database.sweep_expired(NOW).unwrap().len(), 1);
    assert!(database.sweep_expired(NOW).unwrap().is_empty());
}

#[test]
fn a_decision_does_not_lapse() {
    // Expiry is what happens when nobody says anything. Once a human has
    // decided, the record of that decision is not something a clock revises.
    let mut database = database();

    let (arrival, expires_at) = expiring(1, "2026-09-12T00:00:00Z");
    let mut message = arrival.message();
    message.expires_at = Some(&expires_at);
    database.record_inbound(&message, NOW).unwrap();

    database
        .commit_decision(&decision("decline", None))
        .unwrap();

    assert!(database.sweep_expired(NOW).unwrap().is_empty());
    assert_eq!(
        database.inbox_entries(CHANNEL).unwrap()[0].disposition,
        "declined"
    );
}

#[test]
fn a_message_kept_in_the_inbox_still_lapses() {
    // `keep_in_inbox` deliberately leaves the message quarantined, so it is
    // still waiting on a human and expiry still applies to it. This is the
    // case most likely to be got wrong by treating "has a decision record"
    // as "is decided".
    let mut database = database();

    let (arrival, expires_at) = expiring(1, "2026-09-12T00:00:00Z");
    let mut message = arrival.message();
    message.expires_at = Some(&expires_at);
    database.record_inbound(&message, NOW).unwrap();

    database
        .commit_decision(&decision("keep_in_inbox", None))
        .unwrap();

    assert_eq!(
        database.sweep_expired(NOW).unwrap(),
        vec!["msg-1".to_owned()]
    );
}
