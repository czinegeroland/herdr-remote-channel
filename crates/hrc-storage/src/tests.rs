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
