//! Command tests.
//!
//! These drive the command functions directly against a temporary state
//! directory. The end-to-end behavior of the binary — exit codes, `--json`
//! rejection, the authorization boundary — is covered by
//! `tests/command_contract.rs`.
//!
//! Each test builds its own [`Context`], so nothing here depends on the
//! process environment. That matters: the environment is shared by every
//! test in a binary, and a test that mutated it would race the others.

use super::*;
use std::path::Path;
use std::process::Command as ProcessCommand;

use hrc_core::ContextItem;
use hrc_transport::{ObjectClass, PublicationClass, PublishObject, PublishRequest, Transport};
use hrc_transport_git::GitTransport;

/// A fresh state directory and a context that can unlock its key store.
fn home() -> (tempfile::TempDir, Context) {
    let directory = tempfile::tempdir().unwrap();
    let context = Context {
        paths: Paths::at(directory.path().join("state")),
        passphrase: Some(SecretString::from(
            "correct horse battery staple".to_owned(),
        )),
    };
    (directory, context)
}

fn bare_remote(path: &Path) {
    let status = ProcessCommand::new("git")
        .args(["init", "--bare", "--quiet"])
        .arg(path)
        .status()
        .unwrap();
    assert!(status.success());
}

fn peer(root: &Path, name: &str, remote: &Path) -> GitTransport {
    GitTransport::open(root.join(name), remote.to_str().unwrap()).unwrap()
}

fn message(name: &str) -> PublishObject {
    PublishObject {
        name: format!("messages/2026/09/{name}.age"),
        class: ObjectClass::Message,
        bytes: format!("ciphertext-{name}").into_bytes(),
    }
}

fn messaging_home() -> (tempfile::TempDir, Context, String, String) {
    let (directory, context) = home();
    let identity = init(&context).unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);
    let channel = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    (
        directory,
        context,
        channel["channelId"].as_str().unwrap().to_owned(),
        identity["principalKey"].as_str().unwrap().to_owned(),
    )
}

fn admitted_peer(admin: &Context, root: &Path, name: &str) -> (Context, String) {
    let context = Context {
        paths: Paths::at(root.join(name)),
        passphrase: admin.passphrase.clone(),
    };
    let identity = init(&context).unwrap();
    let invite = invite_create(admin, name, "24h").unwrap();
    let joined = join(&context, invite["inviteCode"].as_str().unwrap()).unwrap();
    admit_join(admin, joined["requestId"].as_str().unwrap()).unwrap();
    (
        context,
        identity["principalKey"].as_str().unwrap().to_owned(),
    )
}

#[test]
fn a_successful_send_after_a_refusal_still_arrives() {
    let (_directory, context, channel_id, principal) = messaging_home();
    assert!(send(&context, "not-a-member", "invalid recipient", None).is_err());
    assert!(
        send(
            &context,
            &format!("{principal}/INVALID"),
            "invalid endpoint",
            None
        )
        .is_err()
    );

    let sent = send(&context, &principal, "the next valid message", None).unwrap();
    sync_once(&context).unwrap();
    let database = Database::open(context.paths.database()).unwrap();
    let entries = database.inbox_entries(&channel_id).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message_id, sent["messageId"].as_str().unwrap());
    assert!(database.held_inbound(&channel_id).unwrap().is_empty());
}

#[test]
fn private_conversations_and_receipts_do_not_block_other_recipients() {
    let (directory, alice, channel_id, alice_id) = messaging_home();
    let (bob, bob_id) = admitted_peer(&alice, directory.path(), "bob");
    let (carol, carol_id) = admitted_peer(&alice, directory.path(), "carol");

    let first = send(&alice, &bob_id, "only Bob can read this", None).unwrap();
    let middle = send(&alice, &carol_id, "only Carol can read this", None).unwrap();
    let last = send(&alice, &bob_id, "Bob receives this too", None).unwrap();
    sync_once(&bob).unwrap();
    sync_once(&carol).unwrap();
    sync_once(&alice).unwrap();

    let bob_db = Database::open(bob.paths.database()).unwrap();
    let bob_entries = bob_db.inbox_entries(&channel_id).unwrap();
    assert_eq!(
        bob_entries
            .iter()
            .map(|entry| entry.message_id.as_str())
            .collect::<Vec<_>>(),
        vec![
            first["messageId"].as_str().unwrap(),
            last["messageId"].as_str().unwrap()
        ]
    );
    let carol_db = Database::open(carol.paths.database()).unwrap();
    let carol_entries = carol_db.inbox_entries(&channel_id).unwrap();
    assert_eq!(carol_entries.len(), 1);
    assert_eq!(
        carol_entries[0].message_id,
        middle["messageId"].as_str().unwrap()
    );

    // Bob already sent Alice a receipt. Carol must not need to decrypt it.
    let cross = send(&bob, &carol_id, "a new conversation after a receipt", None).unwrap();
    sync_once(&carol).unwrap();
    assert!(
        carol_db
            .inbox_entries(&channel_id)
            .unwrap()
            .iter()
            .any(|entry| entry.message_id == cross["messageId"].as_str().unwrap()),
        "Carol inbox: {:?}; held: {:?}",
        carol_db.inbox_entries(&channel_id).unwrap(),
        carol_db.held_inbound(&channel_id).unwrap()
    );
    assert!(carol_db.held_inbound(&channel_id).unwrap().is_empty());
    assert!(bob_db.held_inbound(&channel_id).unwrap().is_empty());
    let alice_db = Database::open(alice.paths.database()).unwrap();
    for sent in [&first, &middle, &last] {
        assert!(
            !alice_db
                .receipts_for(sent["messageId"].as_str().unwrap())
                .unwrap()
                .is_empty()
        );
    }
    assert_ne!(alice_id, bob_id);
}

#[test]
fn unlocking_receives_downloaded_messages_without_another_remote_commit() {
    let (_directory, context, channel_id, principal) = messaging_home();
    let sent = send(&context, &principal, "arrived while locked", None).unwrap();
    let locked = Context {
        paths: context.paths.clone(),
        passphrase: None,
    };
    sync_once(&locked).unwrap();
    let database = Database::open(context.paths.database()).unwrap();
    assert!(database.inbox_entries(&channel_id).unwrap().is_empty());

    let unlocked = sync_once(&context).unwrap();
    assert_eq!(unlocked["channels"][0]["remoteChanged"], false);
    let entries = database.inbox_entries(&channel_id).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].message_id, sent["messageId"].as_str().unwrap());
}

#[test]
fn an_expired_predecessor_does_not_hide_the_next_live_message() {
    let (_directory, context, channel_id, principal) = messaging_home();
    let expired = send(&context, &principal, "no longer actionable", Some("1m")).unwrap();
    let live = send(&context, &principal, "still actionable", None).unwrap();
    let mut database = Database::open(context.paths.database()).unwrap();
    let channel = database.channel(&channel_id).unwrap().unwrap();
    let now = expiry_from(&database.utc_now().unwrap(), "2m").unwrap();
    let transport = GitTransport::open(
        context.paths.channel_transport(&channel_id),
        &channel.transport_locator,
    )
    .unwrap();
    let device: DeviceSecrets = context.key_store().unwrap().load(DEVICE_KEY_NAME).unwrap();
    receive_messages(
        &transport,
        &mut database,
        &channel,
        Some(&device.device_identity().unwrap()),
        &now,
    )
    .unwrap();
    database.sweep_expired(&now).unwrap();

    let entries = database.inbox_entries(&channel_id).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(
        entries[0].message_id,
        expired["messageId"].as_str().unwrap()
    );
    assert_eq!(entries[0].disposition, "expired");
    assert_eq!(entries[1].message_id, live["messageId"].as_str().unwrap());
    assert_eq!(entries[1].disposition, "quarantined");
    let pending = database.pending_inbound().unwrap();
    assert_eq!(pending.len(), 1);
    assert!(pending[0].body.contains("still actionable"));
}

#[test]
fn delegation_content_survives_trusted_preview_and_approved_delivery() {
    let (_directory, context, _channel_id, principal) = messaging_home();
    let sent = delegate(
        &context,
        &principal,
        "Review the retry logic",
        "Confirm that retries stop after a minute",
        &["Retries are bounded".into()],
        Some("ctx-already-shared"),
        None,
    )
    .unwrap();
    let message_id = sent["messageId"].as_str().unwrap();
    sync_once(&context).unwrap();
    assert!(show(&context, message_id).unwrap()["body"].is_null());
    let ledger = Mutex::new(hrc_core::rpc::ContextAuthorizationLedger::new());
    let preview = handle_trusted_request(
        &context,
        &ledger,
        TrustedRequest::PreviewPending {
            message_id: message_id.to_owned(),
        },
    );
    assert_eq!(preview["status"], "ok", "{preview}");
    let body: Value = canonical::from_json_str(preview["body"].as_str().unwrap()).unwrap();
    assert_eq!(body["title"], "Review the retry logic");
    assert_eq!(
        body["description"],
        "Confirm that retries stop after a minute"
    );
    assert_eq!(body["acceptanceCriteria"][0], "Retries are bounded");
    assert_eq!(body["contextId"], "ctx-already-shared");

    let approved = handle_trusted_request(
        &context,
        &ledger,
        TrustedRequest::Approve {
            message_id: message_id.to_owned(),
            decision: hrc_core::rpc::WireDecision::DeliverToAgent {
                agent: "reviewer".into(),
            },
            expires_at: "2099-01-01T00:00:00Z".into(),
        },
    );
    assert_eq!(approved["status"], "ok", "{approved}");
    let shown = show(&context, message_id).unwrap();
    let delivered = shown["body"].as_str().unwrap();
    assert!(delivered.contains("Review the retry logic"));
    assert!(delivered.contains("Confirm that retries stop after a minute"));
    assert!(delivered.contains("Retries are bounded"));
}

#[test]
fn a_thread_shows_released_content_and_the_metadata_of_everything_else() {
    // docs/RESEARCH.md 6.4. The thread pane follows the rule `hrc show`
    // follows: content only where a human released it.
    use crate::commands::review::{AWAITING, SENT_HERE, thread_entries};

    let (_directory, context, channel_id, principal) = messaging_home();
    let asked = ask(&context, &principal, "RELEASED_QUESTION", None).unwrap();
    let question_id = asked["messageId"].as_str().unwrap().to_owned();
    sync_once(&context).unwrap();
    let answered = reply(&context, &question_id, "WITHHELD_ANSWER").unwrap();
    sync_once(&context).unwrap();
    let thread_id = answered["threadId"].as_str().unwrap().to_owned();

    let database = Database::open(context.paths.database()).unwrap();
    let before = thread_entries(&database, &channel_id, &thread_id).unwrap();
    let text: String = before
        .iter()
        .map(|entry| format!("{}\n{}\n", entry.heading, entry.body))
        .collect();
    assert!(!text.contains("RELEASED_QUESTION"), "{text}");
    assert!(!text.contains("WITHHELD_ANSWER"), "{text}");
    assert!(text.contains(AWAITING), "{text}");
    assert!(text.contains(SENT_HERE), "{text}");

    let ledger = Mutex::new(hrc_core::rpc::ContextAuthorizationLedger::new());
    let preview = handle_trusted_request(
        &context,
        &ledger,
        TrustedRequest::PreviewPending {
            message_id: question_id.clone(),
        },
    );
    assert_eq!(preview["status"], "ok", "{preview}");
    let approved = handle_trusted_request(
        &context,
        &ledger,
        TrustedRequest::Approve {
            message_id: question_id.clone(),
            decision: hrc_core::rpc::WireDecision::DeliverToAgent {
                agent: "reviewer".into(),
            },
            expires_at: "2099-01-01T00:00:00Z".into(),
        },
    );
    assert_eq!(approved["status"], "ok", "{approved}");

    let after = thread_entries(&database, &channel_id, &thread_id).unwrap();
    let text: String = after
        .iter()
        .map(|entry| format!("{}\n{}\n", entry.heading, entry.body))
        .collect();
    assert!(text.contains("RELEASED_QUESTION"), "{text}");
    assert!(
        !text.contains("WITHHELD_ANSWER"),
        "the answer was never approved: {text}"
    );
}

#[test]
fn wait_returns_the_advanced_report_when_earlier_milestones_are_reached() {
    let (_directory, context, channel_id, principal) = messaging_home();
    let sent = send(
        &context,
        &principal,
        "a message with advanced receipts",
        None,
    )
    .unwrap();
    let message_id = sent["messageId"].as_str().unwrap();
    let database = Database::open(context.paths.database()).unwrap();
    let now = database.utc_now().unwrap();
    for state in ["delivered", "read", "accepted"] {
        database
            .record_receipt(
                &channel_id,
                &hrc_storage::RecordedReceipt {
                    message_id: message_id.into(),
                    reporter_principal: principal.clone(),
                    reporter_device: "reporting-device".into(),
                    state: state.into(),
                    rejection_code: None,
                    reported_at: now.clone(),
                },
                &now,
            )
            .unwrap();
        for wanted in ["published", "delivered"] {
            let waited = wait(&context, message_id, Some(wanted), Some("1s")).unwrap();
            assert_eq!(waited["state"], state);
        }
    }
    database
        .record_receipt(
            &channel_id,
            &hrc_storage::RecordedReceipt {
                message_id: message_id.into(),
                reporter_principal: principal,
                reporter_device: "rejecting-device".into(),
                state: "rejected".into(),
                rejection_code: Some("declined".into()),
                reported_at: now.clone(),
            },
            &now,
        )
        .unwrap();
    assert_eq!(
        wait(&context, message_id, Some("delivered"), Some("1s")).unwrap()["state"],
        "accepted"
    );
    assert!(!state_reaches("rejected", "accepted"));
    assert!(!state_reaches("rejected", "delivered"));
    assert!(!state_reaches("declined", "approved"));
    assert!(!state_reaches("published", "delivered"));
}

#[test]
fn delivery_receipts_are_partitioned_by_originating_device() {
    let entry = |message_id: &str, sender_device: &str| hrc_storage::InboxEntry {
        message_id: message_id.into(),
        sender_principal: "same-principal".into(),
        sender_device: sender_device.into(),
        kind: "note".into(),
        thread_id: Some(message_id.into()),
        in_reply_to: None,
        arrival_sequence: 1,
        expires_at: None,
        disposition: "quarantined".into(),
    };
    let obligations = delivery_receipt_obligations(
        vec![entry("from-a", "device-a"), entry("from-b", "device-b")],
        &std::collections::HashSet::new(),
    );

    assert_eq!(obligations.len(), 2);
    assert_eq!(
        obligations[&("same-principal".into(), "device-a".into())],
        ["from-a"]
    );
    assert_eq!(
        obligations[&("same-principal".into(), "device-b".into())],
        ["from-b"]
    );
}

#[test]
fn wait_timeouts_are_bounded_and_malformed_durations_do_not_panic() {
    let (_directory, context, _channel_id, principal) = messaging_home();
    let sent = send(&context, &principal, "not yet accepted", None).unwrap();
    let started = std::time::Instant::now();
    let waited = wait(
        &context,
        sent["messageId"].as_str().unwrap(),
        Some("accepted"),
        Some("1s"),
    )
    .unwrap();
    assert_eq!(waited["state"], "timeout");
    assert!(started.elapsed() < Duration::from_secs(3));
    for value in [
        "1\u{00e9}",
        "18446744073709551615h",
        "18446744073709551615m",
        "0s",
    ] {
        assert!(parse_wait_timeout(value).is_err(), "{value}");
    }
}

#[test]
fn a_failed_receipt_is_retried_without_redelivering_the_message() {
    let (directory, context, channel_id, principal) = messaging_home();
    let sent = send(
        &context,
        &principal,
        "receipt must survive a failed attempt",
        None,
    )
    .unwrap();
    let database = Database::open(context.paths.database()).unwrap();
    let channel = database.channel(&channel_id).unwrap().unwrap();
    let now = database.utc_now().unwrap();
    receive_for(&context, &channel, &now).unwrap();

    let remote = directory.path().join("remote.git");
    let offline = directory.path().join("offline.git");
    std::fs::rename(&remote, &offline).unwrap();
    assert!(publish_delivery_receipts(&context, &channel, &now).is_err());
    std::fs::rename(&offline, &remote).unwrap();
    sync_once(&context).unwrap();
    sync_once(&context).unwrap();
    assert_eq!(database.inbox_entries(&channel_id).unwrap().len(), 1);
    assert!(
        !database
            .receipts_for(sent["messageId"].as_str().unwrap())
            .unwrap()
            .is_empty(),
        "sent facts: {:?}; held: {:?}; audit: {:?}",
        database.sent_messages(&channel_id).unwrap(),
        database.held_inbound(&channel_id).unwrap(),
        database.audit_entries(None).unwrap()
    );
}

#[test]
fn init_creates_keys_and_state() {
    let (_directory, context) = home();

    let value = init(&context).unwrap();

    assert_eq!(value["status"], "ok");
    assert!(
        value["signingKey"].as_str().unwrap().len() > 20,
        "a signing key should be reported"
    );
    assert!(
        value["encryptionRecipient"]
            .as_str()
            .unwrap()
            .starts_with("age1")
    );

    assert!(context.paths.database().is_file());
    assert!(context.paths.keys().is_dir());
}

#[test]
fn a_backlog_larger_than_one_receipt_is_reported_completely() {
    let (_directory, context, channel_id, principal) = messaging_home();
    let mut database = Database::open(context.paths.database()).unwrap();
    let channel = database.channel(&channel_id).unwrap().unwrap();
    let now = database.utc_now().unwrap();
    let transport = GitTransport::open(
        context.paths.channel_transport(&channel_id),
        &channel.transport_locator,
    )
    .unwrap();
    let (roster, _) = load_roster(&transport, &channel_id).unwrap();
    let device: DeviceSecrets = context.key_store().unwrap().load(DEVICE_KEY_NAME).unwrap();
    let device_id = local_device_id(&roster, &device).unwrap();
    let count = hrc_protocol::receipt::MAX_REFERENCED_MESSAGES + 1;
    let mut previous = None;
    let mut expected = std::collections::BTreeSet::new();
    for sequence in 1..=count {
        let message_id = format!("backlog-{sequence}");
        let chain_id =
            hrc_storage::chain_id_for(&channel_id, &device_id, sequence as u64, &message_id)
                .unwrap();
        database
            .record_inbound(
                &hrc_storage::InboundMessage {
                    channel_id: &channel_id,
                    message_id: &message_id,
                    sender_principal: &principal,
                    sender_device: &device_id,
                    device_sequence: sequence as u64,
                    chain_id: &chain_id,
                    previous_chain_id: previous.as_deref(),
                    recipient_device: None,
                    recipient_previous_chain_id: None,
                    kind: "note",
                    thread_id: Some(&message_id),
                    in_reply_to: None,
                    roster_epoch: roster.epoch(),
                    endpoint: None,
                    ciphertext_bytes: 1,
                    plaintext_bytes: 1,
                    ciphertext_sha256: &message_id,
                    created_at: &now,
                    expires_at: None,
                    expired: false,
                    attachment_count: 0,
                    attachment_bytes: 0,
                    prompt_request: false,
                    body: b"message already accepted",
                    ciphertext: b"ciphertext already verified",
                    context: None,
                },
                &now,
            )
            .unwrap();
        expected.insert(message_id);
        previous = Some(chain_id);
    }

    publish_delivery_receipts(&context, &channel, &now).unwrap();
    let mut reported = std::collections::BTreeSet::new();
    let mut receipts = 0;
    let identity = device.device_identity().unwrap();
    for publication in transport.fetch(None, 100).unwrap().publications {
        for object in publication.objects {
            if object.class != ObjectClass::Message {
                continue;
            }
            let bytes = transport.get_object(&object.name, &object.sha256).unwrap();
            let opened =
                hrc_core::message::open(&roster, &identity, &bytes, roster.epoch(), &now).unwrap();
            assert_eq!(opened.envelope.kind, "receipt");
            let receipt: hrc_protocol::receipt::ReceiptBody =
                serde_json::from_value(opened.envelope.body).unwrap();
            receipt.validate().unwrap();
            assert!(
                receipt.referenced_message_ids.len()
                    <= hrc_protocol::receipt::MAX_REFERENCED_MESSAGES
            );
            for message_id in receipt.referenced_message_ids {
                assert!(
                    reported.insert(message_id),
                    "a batch repeated an identifier"
                );
            }
            receipts += 1;
        }
    }
    assert_eq!(receipts, 2);
    assert_eq!(reported, expected);
    let head = transport.remote_head().unwrap();
    publish_delivery_receipts(&context, &channel, &now).unwrap();
    assert_eq!(transport.remote_head().unwrap(), head);
}

#[test]
fn init_refuses_to_overwrite_an_existing_identity() {
    // Overwriting would discard the identity every channel knows this
    // installation by, orphaning it silently.
    let (_directory, context) = home();
    init(&context).unwrap();

    assert!(matches!(
        init(&context).unwrap_err(),
        CliError::AlreadyInitialized
    ));
}

#[test]
fn whoami_reports_the_same_identity_init_created() {
    let (_directory, context) = home();
    let created = init(&context).unwrap();
    let reported = whoami(&context).unwrap();

    assert_eq!(created["signingKey"], reported["signingKey"]);
    assert_eq!(
        created["encryptionRecipient"],
        reported["encryptionRecipient"]
    );
}

#[test]
fn whoami_before_init_reports_no_stored_key() {
    let (_directory, context) = home();
    context.paths.ensure().unwrap();

    assert!(matches!(
        whoami(&context).unwrap_err(),
        CliError::Crypto(hrc_crypto::CryptoError::NoStoredKey { .. })
    ));
}

#[test]
fn no_command_output_contains_secret_material() {
    // The identity is public; the key behind it is not. Nothing a command
    // prints may include private material (PRD sections 14.1 and 29).
    let (_directory, context) = home();

    let created = init(&context).unwrap();
    let rendered = format!("{created}{}", whoami(&context).unwrap());

    assert!(!rendered.contains("AGE-SECRET-KEY"));
    assert!(!rendered.to_lowercase().contains("passphrase"));
    assert!(!rendered.contains("correct horse battery staple"));
}

#[test]
fn channels_is_empty_before_any_channel_exists() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let value = channels(&context).unwrap();
    assert_eq!(value["status"], "ok");
    assert!(value["channels"].as_array().unwrap().is_empty());
}

#[test]
fn status_reports_the_fields_the_prd_requires() {
    // PRD section 29 lists what `hrc status` must show.
    let (_directory, context) = home();
    init(&context).unwrap();

    let database = Database::open(context.paths.database()).unwrap();
    database
        .insert_channel(
            "channel-1",
            "git",
            "owner/channel",
            "Test channel",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database.set_roster_progress("channel-1", 3, 7).unwrap();
    database.set_sync_cursor("channel-1", "rev-abc").unwrap();
    drop(database);

    let value = status(&context).unwrap();
    let channel = &value["channels"][0];

    assert_eq!(channel["channelId"], "channel-1");
    assert_eq!(channel["transport"], "git");
    assert_eq!(channel["locator"], "owner/channel");
    assert_eq!(channel["rosterEpoch"], 3);
    assert_eq!(channel["controlSequence"], 7);
    assert_eq!(channel["lastFetchedRevision"], "rev-abc");
    assert_eq!(channel["outboxPending"], 0);
    assert_eq!(channel["unread"], 0);
    assert_eq!(channel["pendingApproval"], 0);
}

#[test]
fn status_counts_pending_outbox_entries() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let mut database = Database::open(context.paths.database()).unwrap();
    database
        .insert_channel(
            "channel-1",
            "git",
            "owner/channel",
            "Test channel",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .allocate_outgoing(
            "channel-1",
            "device-1",
            "msg-1",
            1,
            "hash",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .queue_outgoing("msg-1", b"ciphertext", "2026-09-13T00:00:00Z")
        .unwrap();
    drop(database);

    let value = status(&context).unwrap();
    assert_eq!(value["channels"][0]["outboxPending"], 1);
}

#[test]
fn status_surfaces_a_halted_channel() {
    // A halted channel is the single most important thing status can say,
    // so it must appear rather than being inferable from a missing cursor.
    let (_directory, context) = home();
    init(&context).unwrap();

    let database = Database::open(context.paths.database()).unwrap();
    database
        .insert_channel(
            "channel-1",
            "git",
            "owner/channel",
            "Test channel",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .halt_channel("channel-1", "history rewritten")
        .unwrap();
    drop(database);

    let value = status(&context).unwrap();
    assert_eq!(value["channels"][0]["haltedReason"], "history rewritten");

    let listed = channels(&context).unwrap();
    assert_eq!(listed["channels"][0]["halted"], true);
}

#[test]
fn doctor_reports_every_check_rather_than_stopping_at_the_first() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let value = diagnose(&context).unwrap().report();
    let checks = value["checks"].as_array().unwrap();

    let names: Vec<&str> = checks
        .iter()
        .map(|check| check["check"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"protocol_version"));
    assert!(names.contains(&"git"));
    assert!(names.contains(&"state_directory"));
    assert!(names.contains(&"database"));
    assert!(names.contains(&"device_keys"));

    assert_eq!(value["healthy"], true, "a fresh install should be healthy");
    assert_eq!(value["status"], "ok");
}

#[test]
fn doctor_is_unhealthy_before_init_but_still_reports_everything() {
    // Diagnosing a broken installation needs the whole picture, not the
    // first symptom.
    let (_directory, context) = home();
    context.paths.ensure().unwrap();

    let value = diagnose(&context).unwrap().report();

    assert_eq!(value["healthy"], false);
    assert_eq!(value["status"], "unhealthy");
    assert_eq!(value["checks"].as_array().unwrap().len(), 5);

    let keys = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "device_keys")
        .unwrap();
    assert_eq!(keys["ok"], false);
    assert!(
        keys["detail"].as_str().unwrap().contains("hrc init"),
        "the failure should say how to fix it"
    );
}

#[test]
fn the_daemon_and_the_exit_code_read_the_same_checks_the_report_prints() {
    // docs/REFACTOR.md R9: the daemon used to parse `hrc doctor`'s printed
    // JSON for its health summary. Both now read the typed checks, and the
    // report is rendered from them, so the three cannot disagree.
    let (_directory, context) = home();
    context.paths.ensure().unwrap();

    let diagnosis = diagnose(&context).unwrap();
    let report = diagnosis.report();

    assert!(!diagnosis.healthy(), "no device keys yet");
    assert_eq!(report["healthy"], diagnosis.healthy());

    let printed: Vec<(String, bool)> = report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|check| {
            (
                check["check"].as_str().unwrap().to_owned(),
                check["ok"].as_bool().unwrap(),
            )
        })
        .collect();
    let typed: Vec<(String, bool)> = diagnosis
        .checks
        .iter()
        .map(|check| (check.name.to_owned(), check.ok))
        .collect();
    assert_eq!(printed, typed);
}

#[test]
fn doctor_finds_the_git_executable() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let value = diagnose(&context).unwrap().report();
    let git = value["checks"]
        .as_array()
        .unwrap()
        .iter()
        .find(|check| check["check"] == "git")
        .unwrap();

    assert_eq!(git["ok"], true, "git is required and is present in CI");
    assert!(git["detail"].as_str().unwrap().starts_with("git version"));
}

#[test]
fn audit_is_empty_before_any_action_is_recorded() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let value = audit(&context, None).unwrap();
    assert_eq!(value["status"], "ok");
    assert!(value["entries"].as_array().unwrap().is_empty());
}

#[test]
fn audit_reports_entries_newest_first_and_can_filter_by_time() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let database = Database::open(context.paths.database()).unwrap();
    database
        .append_audit(
            Some("channel-1"),
            Some("msg-1"),
            "draft_saved",
            Some("hash-1"),
            Some("first"),
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .append_audit(
            Some("channel-1"),
            Some("msg-2"),
            "synchronization_halted",
            None,
            Some("history rewritten"),
            "2026-09-13T01:00:00Z",
        )
        .unwrap();

    let value = audit(&context, None).unwrap();
    let entries = value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0]["action"], "synchronization_halted");
    assert_eq!(entries[0]["detail"], "history rewritten");
    assert_eq!(entries[1]["action"], "draft_saved");

    let filtered = audit(&context, Some("2026-09-13T00:30:00Z")).unwrap();
    let entries = filtered["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["messageId"], "msg-2");
}

#[test]
fn sync_once_fetches_only_when_the_remote_head_changed() {
    let (directory, context) = home();
    init(&context).unwrap();

    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let created = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap();
    let mut publisher = peer(directory.path(), "publisher", &remote);
    publisher.sync_from_remote().unwrap();
    let genesis = publisher.open_group().unwrap();
    let message = publisher
        .publish(PublishRequest {
            expected_revision: genesis.revision.clone(),
            class: PublicationClass::Data,
            objects: vec![message("msg-1")],
        })
        .unwrap();

    let first = sync_once(&context).unwrap();
    assert_eq!(first["status"], "ok");
    assert_eq!(first["channels"][0]["remoteChanged"], true);
    assert_eq!(first["channels"][0]["fetchedPublications"], 2);
    assert_eq!(
        Database::open(context.paths.database())
            .unwrap()
            .channel(channel_id)
            .unwrap()
            .unwrap()
            .sync_cursor
            .as_deref(),
        Some(message.revision.as_str())
    );

    let second = sync_once(&context).unwrap();
    assert_eq!(second["channels"][0]["remoteChanged"], false);
    assert_eq!(second["channels"][0]["fetchedPublications"], 0);
}

#[test]
fn sync_once_publishes_queued_messages() {
    let (directory, context) = home();
    init(&context).unwrap();

    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let created = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap();
    let publisher = peer(directory.path(), "publisher", &remote);
    publisher.sync_from_remote().unwrap();
    let genesis = publisher.open_group().unwrap();

    let mut database = Database::open(context.paths.database()).unwrap();
    database
        .set_sync_cursor(channel_id, genesis.revision.as_deref().unwrap())
        .unwrap();
    database
        .allocate_outgoing(
            channel_id,
            "device-1",
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            0,
            "hash",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .queue_outgoing(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            b"ciphertext-queued",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    drop(database);

    let value = sync_once(&context).unwrap();
    assert_eq!(
        value["channels"][0]["publishedMessages"][0],
        "01ARZ3NDEKTSV4RRFFQ69G5FAV"
    );
    assert_eq!(
        Database::open(context.paths.database())
            .unwrap()
            .outbox_state("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .unwrap(),
        Some(hrc_storage::OutboxState::Published)
    );

    let reader = peer(directory.path(), "reader", &remote);
    reader.sync_from_remote().unwrap();
    let page = reader.fetch(genesis.revision.as_deref(), 100).unwrap();
    assert_eq!(page.publications.len(), 1);
    let object = &page.publications[0].objects[0];
    assert_eq!(
        object.name,
        "messages/2026/09/01ARZ3NDEKTSV4RRFFQ69G5FAV.age"
    );
    assert_eq!(
        reader.get_object(&object.name, &object.sha256).unwrap(),
        b"ciphertext-queued"
    );
}

#[test]
fn stale_queued_ciphertext_waits_for_keys_then_reencrypts_for_the_new_roster() {
    let (directory, context) = home();
    init(&context).unwrap();

    let remote = directory.path().join("remote.git");
    bare_remote(&remote);
    let created = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap().to_owned();

    let store = context.key_store().unwrap();
    let device: DeviceSecrets = store.load(DEVICE_KEY_NAME).unwrap();
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME).unwrap();
    let transport = peer(directory.path(), "old-roster", &remote);
    transport.sync_from_remote().unwrap();
    let (old_roster, _) = load_roster(&transport, &channel_id).unwrap();
    let device_id = local_device_id(&old_roster, &device).unwrap();

    let message_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let created_at = "2026-09-13T00:00:00Z";
    let body = json!({ "text": "survives a roster change" });
    let payload_hash = canonical::canonical_sha256_hex(&body).unwrap();
    let mut database = Database::open(context.paths.database()).unwrap();
    let reservation = database
        .allocate_outgoing(
            &channel_id,
            &device_id,
            message_id,
            old_roster.epoch(),
            &payload_hash,
            created_at,
        )
        .unwrap();
    let envelope = MessageEnvelope {
        version: hrc_protocol::PROTOCOL_VERSION,
        channel_id: channel_id.clone(),
        roster_epoch: old_roster.epoch(),
        message_id: message_id.to_owned(),
        device_sequence: reservation.device_sequence,
        previous_chain_id: reservation.previous_chain_id.clone(),
        created_at: created_at.to_owned(),
        expires_at: None,
        to: hrc_protocol::Addressing {
            principals: Vec::new(),
            endpoint: Some("reviewer".into()),
        },
        recipients: hrc_protocol::RecipientDevices::new([device_id.clone()]).unwrap(),
        recipient_previous_chain_ids: None,
        thread_id: "thread-before-epoch-change".into(),
        in_reply_to: Some("earlier-message".into()),
        kind: hrc_protocol::MessageKind::Note.as_str().into(),
        requested_capability: None,
        body: body.clone(),
        attachments: Vec::new(),
        padding: String::new(),
    };
    let old_ciphertext = hrc_core::message::seal(
        &old_roster,
        &device.signing_key(),
        Signer {
            principal_id: principal.signing_key().verifying_key().to_base64url(),
            device_id: device_id.clone(),
        },
        envelope.clone(),
    )
    .unwrap();
    let reseal_material = hrc_crypto::encrypt_to(
        &[device.device_identity().unwrap().recipient()],
        &canonical::to_canonical_bytes(&envelope).unwrap(),
    )
    .unwrap();
    database
        .queue_outgoing_resealable(
            message_id,
            &old_ciphertext,
            Some(&reseal_material),
            created_at,
        )
        .unwrap();
    database
        .record_sent_facts(
            message_id,
            &envelope.thread_id,
            &envelope.kind,
            std::slice::from_ref(&device_id),
        )
        .unwrap();
    drop(database);

    let joiner_directory = tempfile::tempdir().unwrap();
    let joiner = Context {
        paths: Paths::at(joiner_directory.path().join("state")),
        passphrase: context.passphrase.clone(),
    };
    init(&joiner).unwrap();
    let invite = invite_create(&context, "bob", "24h").unwrap();
    let joined = join(&joiner, invite["inviteCode"].as_str().unwrap()).unwrap();
    admit_join(&context, joined["requestId"].as_str().unwrap()).unwrap();

    let locked = Context {
        paths: context.paths.clone(),
        passphrase: None,
    };
    let deferred = daemon_tick(&locked).unwrap();
    assert_eq!(deferred["channels"][0]["deferredMessages"][0], message_id);
    let queued = Database::open(context.paths.database())
        .unwrap()
        .pending_outgoing_records(&channel_id)
        .unwrap()
        .remove(0);
    assert_eq!(queued.roster_epoch, old_roster.epoch());
    assert_eq!(queued.ciphertext, old_ciphertext);
    let before_unlock = peer(directory.path(), "before-unlock", &remote);
    before_unlock.sync_from_remote().unwrap();
    assert!(
        before_unlock
            .fetch(None, 100)
            .unwrap()
            .publications
            .iter()
            .flat_map(|publication| &publication.objects)
            .all(|object| object.class != ObjectClass::Message),
        "stale-epoch ciphertext was published while the key store was locked"
    );

    let published = sync_once(&context).unwrap();
    assert_eq!(published["channels"][0]["publishedMessages"][0], message_id);

    let reader = peer(directory.path(), "new-roster", &remote);
    reader.sync_from_remote().unwrap();
    let (new_roster, _) = load_roster(&reader, &channel_id).unwrap();
    assert_eq!(new_roster.epoch(), old_roster.epoch() + 1);
    let page = reader.fetch(None, 100).unwrap();
    let object = page
        .publications
        .iter()
        .flat_map(|publication| &publication.objects)
        .find(|object| object.class == ObjectClass::Message)
        .unwrap();
    let new_ciphertext = reader.get_object(&object.name, &object.sha256).unwrap();
    assert_ne!(new_ciphertext, old_ciphertext);

    let joiner_store = joiner.key_store().unwrap();
    let joiner_device: DeviceSecrets = joiner_store.load(DEVICE_KEY_NAME).unwrap();
    assert!(
        joiner_device
            .device_identity()
            .unwrap()
            .decrypt(&old_ciphertext)
            .is_err(),
        "the newly admitted device unexpectedly opened stale ciphertext"
    );
    let opened = hrc_core::message::open(
        &new_roster,
        &joiner_device.device_identity().unwrap(),
        &new_ciphertext,
        new_roster.epoch(),
        created_at,
    )
    .unwrap();
    assert_eq!(opened.envelope.roster_epoch, new_roster.epoch());
    assert_eq!(opened.envelope.message_id, message_id);
    assert_eq!(opened.envelope.device_sequence, reservation.device_sequence);
    assert_eq!(
        opened.envelope.previous_chain_id,
        reservation.previous_chain_id
    );
    assert_eq!(opened.envelope.thread_id, envelope.thread_id);
    assert_eq!(opened.envelope.in_reply_to, envelope.in_reply_to);
    assert_eq!(
        canonical::canonical_sha256_hex(&opened.envelope.body).unwrap(),
        payload_hash
    );

    sync_once(&joiner).unwrap();
    sync_once(&context).unwrap();
    let database = Database::open(context.paths.database()).unwrap();
    let new_device_id = local_device_id(&new_roster, &joiner_device).unwrap();
    assert!(
        database
            .sent_messages(&channel_id)
            .unwrap()
            .iter()
            .find(|sent| sent.message_id == message_id)
            .unwrap()
            .recipient_device_ids
            .contains(&new_device_id)
    );
    assert!(
        database.receipts_for(message_id).unwrap().iter()
            .any(|receipt| receipt.reporter_device == new_device_id && receipt.state == "delivered"),
        "the newly addressed device's real receipt must be accepted after resealing; sent facts: {:?}; held: {:?}; audit: {:?}",
        database.sent_messages(&channel_id).unwrap(),
        database.held_inbound(&channel_id).unwrap(),
        database.audit_entries(None).unwrap()
    );
}

#[test]
fn an_acknowledgement_lost_before_an_epoch_change_does_not_reseal_published_bytes() {
    let (directory, context) = home();
    init(&context).unwrap();

    let remote = directory.path().join("remote.git");
    bare_remote(&remote);
    let created = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap().to_owned();
    let created_at = "2026-09-13T00:00:00Z";
    let message_id = "01ARZ3NDEKTSV4RRFFQ69G5FAV";
    let ciphertext = b"published-before-local-ack";

    let store = context.key_store().unwrap();
    let device: DeviceSecrets = store.load(DEVICE_KEY_NAME).unwrap();
    let mut publisher = peer(directory.path(), "lost-ack-publisher", &remote);
    publisher.sync_from_remote().unwrap();
    let (old_roster, _) = load_roster(&publisher, &channel_id).unwrap();
    let device_id = local_device_id(&old_roster, &device).unwrap();

    let mut database = Database::open(context.paths.database()).unwrap();
    database
        .allocate_outgoing(
            &channel_id,
            &device_id,
            message_id,
            old_roster.epoch(),
            "payload-hash",
            created_at,
        )
        .unwrap();
    database
        .queue_outgoing(message_id, ciphertext, created_at)
        .unwrap();
    drop(database);

    let head = publisher.open_group().unwrap().revision;
    publisher
        .publish(PublishRequest {
            expected_revision: head,
            class: PublicationClass::Data,
            objects: vec![outgoing_message_object(
                message_id,
                created_at,
                ciphertext.to_vec(),
            )],
        })
        .unwrap();

    let joiner_directory = tempfile::tempdir().unwrap();
    let joiner = Context {
        paths: Paths::at(joiner_directory.path().join("state")),
        passphrase: context.passphrase.clone(),
    };
    init(&joiner).unwrap();
    let invite = invite_create(&context, "bob", "24h").unwrap();
    let joined = join(&joiner, invite["inviteCode"].as_str().unwrap()).unwrap();
    admit_join(&context, joined["requestId"].as_str().unwrap()).unwrap();

    let before = peer(directory.path(), "before-lost-ack-sync", &remote);
    before.sync_from_remote().unwrap();
    let publications_before = before.fetch(None, 100).unwrap().publications.len();

    let result = sync_once(&context).unwrap();
    assert_eq!(result["channels"][0]["publishedMessages"][0], message_id);
    assert_eq!(
        Database::open(context.paths.database())
            .unwrap()
            .outbox_state(message_id)
            .unwrap(),
        Some(hrc_storage::OutboxState::Published)
    );

    let after = peer(directory.path(), "after-lost-ack-sync", &remote);
    after.sync_from_remote().unwrap();
    assert_eq!(
        after.fetch(None, 100).unwrap().publications.len(),
        publications_before
    );
}

#[test]
fn an_unresealable_predecessor_blocks_later_messages_from_the_same_device() {
    let (directory, context) = home();
    init(&context).unwrap();

    let remote = directory.path().join("remote.git");
    bare_remote(&remote);
    let created = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap().to_owned();

    let store = context.key_store().unwrap();
    let device: DeviceSecrets = store.load(DEVICE_KEY_NAME).unwrap();
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME).unwrap();
    let transport = peer(directory.path(), "legacy-roster", &remote);
    transport.sync_from_remote().unwrap();
    let (old_roster, _) = load_roster(&transport, &channel_id).unwrap();
    let device_id = local_device_id(&old_roster, &device).unwrap();

    let mut database = Database::open(context.paths.database()).unwrap();
    database
        .allocate_outgoing(
            &channel_id,
            &device_id,
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            old_roster.epoch(),
            "legacy-payload-hash",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    database
        .queue_outgoing(
            "01ARZ3NDEKTSV4RRFFQ69G5FAV",
            b"legacy-ciphertext",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();
    drop(database);

    let joiner_directory = tempfile::tempdir().unwrap();
    let joiner = Context {
        paths: Paths::at(joiner_directory.path().join("state")),
        passphrase: context.passphrase.clone(),
    };
    init(&joiner).unwrap();
    let invite = invite_create(&context, "bob", "24h").unwrap();
    let joined = join(&joiner, invite["inviteCode"].as_str().unwrap()).unwrap();
    admit_join(&context, joined["requestId"].as_str().unwrap()).unwrap();

    let recipient = principal.signing_key().verifying_key().to_base64url();
    let sent = send(&context, &recipient, "must wait for the predecessor", None).unwrap();
    assert_eq!(sent["published"], false);
    assert_eq!(sent["deferred"], true);

    let database = Database::open(context.paths.database()).unwrap();
    assert_eq!(database.pending_outgoing(&channel_id).unwrap().len(), 2);
    assert!(
        database
            .channel_counts(&channel_id)
            .unwrap()
            .last_error
            .unwrap()
            .contains("predates re-encryption support")
    );

    let reader = peer(directory.path(), "blocked-descendant-reader", &remote);
    reader.sync_from_remote().unwrap();
    assert!(
        reader
            .fetch(None, 100)
            .unwrap()
            .publications
            .iter()
            .flat_map(|publication| &publication.objects)
            .all(|object| object.class != ObjectClass::Message)
    );
}

#[test]
fn a_signed_malformed_context_is_rejected_without_halting_the_channel() {
    let (directory, context) = home();
    init(&context).unwrap();
    let remote = directory.path().join("remote.git");
    bare_remote(&remote);
    let created = create(&context, remote.to_str().unwrap(), Some("Test channel")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap().to_owned();

    let store = context.key_store().unwrap();
    let principal: PrincipalSecrets = store.load(PRINCIPAL_KEY_NAME).unwrap();
    let recipient = principal.signing_key().verifying_key().to_base64url();
    let package = hrc_core::ContextPackage::new(
        "ctx-tampered",
        vec![hrc_core::ContextItem::Note {
            text: "the signed context was replaced".into(),
        }],
    );
    let sent = compose_body(
        &context,
        hrc_protocol::MessageKind::Note,
        &recipient,
        json!({
            "context": package,
            "contextDigest": "0".repeat(64),
        }),
        None,
        None,
    )
    .unwrap();
    assert_eq!(sent["published"], true);

    assert!(sync_once(&context).is_ok());
    let database = Database::open(context.paths.database()).unwrap();
    let channel = database.channel(&channel_id).unwrap().unwrap();
    assert!(
        channel.halted_reason.is_none(),
        "a signed malformed context is sender content, not history tampering"
    );
    let entry = database
        .inbox_entries(&channel_id)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    assert_eq!(entry.disposition, "unsupported");
    assert!(
        database
            .audit_entries(None)
            .unwrap()
            .iter()
            .any(|entry| entry.action == "malformed_context"),
        "the local rejection must remain auditable"
    );
}

#[test]
fn context_excerpts_are_source_derived_and_detect_changes_before_trusted_send() {
    let (directory, context) = home();
    init(&context).unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir_all(source.join("src")).unwrap();
    std::fs::write(
        source.join("src/lib.rs"),
        "first line\nchecked source\nlast line\n",
    )
    .unwrap();
    let status = ProcessCommand::new("git")
        .args(["init", "--quiet"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let manifest = source.join("context.json");
    std::fs::write(
        &manifest,
        r#"{"version":1,"id":"ctx-source","items":[{"kind":"excerpt","path":"src/lib.rs","firstLine":2,"lastLine":2,"text":"caller-controlled text"}]}"#,
    )
    .unwrap();

    let drafted = context_draft(
        &context,
        manifest.to_str().unwrap(),
        Some(source.to_str().unwrap()),
    )
    .unwrap();
    assert!(!drafted.to_string().contains("checked source"));
    let database = Database::open(context.paths.database()).unwrap();
    let stored = database.context_draft("ctx-source").unwrap().unwrap();
    assert!(
        std::path::Path::new(stored.repository_root.as_deref().unwrap()).is_absolute(),
        "the durable source identity must be an absolute worktree root"
    );
    let package: ContextPackage =
        canonical::from_json_str(std::str::from_utf8(&stored.manifest).unwrap()).unwrap();
    let ContextItem::Excerpt { text, .. } = &package.items[0] else {
        panic!("expected an excerpt");
    };
    assert_eq!(text, "checked source");
    assert_ne!(text, "caller-controlled text");
    drop(database);

    std::fs::write(
        source.join("src/lib.rs"),
        "first line\nchanged source\nlast line\n",
    )
    .unwrap();
    assert!(matches!(
        context_preview(&context, "ctx-source"),
        Err(CliError::InvalidContextSource { .. })
    ));
}

#[test]
fn context_draft_rejects_an_ignored_source_before_reading_it() {
    let (directory, context) = home();
    init(&context).unwrap();
    let source = directory.path().join("source");
    std::fs::create_dir_all(source.join("private")).unwrap();
    std::fs::write(source.join(".gitignore"), "private/secret.txt\n").unwrap();
    std::fs::write(
        source.join("private").join("secret.txt"),
        "must never be read",
    )
    .unwrap();
    let status = ProcessCommand::new("git")
        .args(["init", "--quiet"])
        .arg(&source)
        .status()
        .unwrap();
    assert!(status.success());
    let manifest = source.join("context.json");
    std::fs::write(
        &manifest,
        r#"{"version":1,"id":"ctx-ignored","items":[{"kind":"excerpt","path":"private\\secret.txt","firstLine":1,"lastLine":1,"text":"must never be read"}]}"#,
    )
    .unwrap();

    let error = context_draft(
        &context,
        manifest.to_str().unwrap(),
        Some(source.to_str().unwrap()),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        CliError::Core(hrc_core::CoreError::ContextContainsExcludedPath { .. })
    ));
    assert!(!error.to_string().contains("must never be read"));
}

#[test]
fn committed_context_excerpt_accepts_sha1_and_sha256_commit_ids() {
    let (directory, context) = home();
    init(&context).unwrap();
    for (object_format, expected_length) in [("sha1", 40), ("sha256", 64)] {
        let source = directory.path().join(format!("source-{object_format}"));
        std::fs::create_dir_all(source.join("src")).unwrap();
        std::fs::write(source.join("src/lib.rs"), "committed source\n").unwrap();
        let mut initialize = ProcessCommand::new("git");
        initialize.args(["init", "--quiet"]);
        if object_format == "sha256" {
            initialize.arg("--object-format=sha256");
        }
        assert!(initialize.arg(&source).status().unwrap().success());
        assert!(
            ProcessCommand::new("git")
                .args(["-C", source.to_str().unwrap(), "add", "src/lib.rs"])
                .status()
                .unwrap()
                .success()
        );
        assert!(
            ProcessCommand::new("git")
                .args([
                    "-C",
                    source.to_str().unwrap(),
                    "-c",
                    "user.name=test",
                    "-c",
                    "user.email=test@example.invalid",
                    "commit",
                    "--quiet",
                    "-m",
                    "source",
                ])
                .status()
                .unwrap()
                .success()
        );
        let commit = String::from_utf8(
            ProcessCommand::new("git")
                .args(["-C", source.to_str().unwrap(), "rev-parse", "HEAD"])
                .output()
                .unwrap()
                .stdout,
        )
        .unwrap();
        assert_eq!(commit.trim().len(), expected_length);
        std::fs::write(source.join("src/lib.rs"), "uncommitted replacement\n").unwrap();
        let context_id = format!("ctx-{object_format}");
        let manifest = source.join("context.json");
        std::fs::write(
            &manifest,
            format!(
                r#"{{"version":1,"id":"{context_id}","items":[{{"kind":"excerpt","path":"src/lib.rs","firstLine":1,"lastLine":1,"commitSha":"{}","text":"caller text"}}]}}"#,
                commit.trim()
            ),
        )
        .unwrap();

        context_draft(
            &context,
            manifest.to_str().unwrap(),
            Some(source.to_str().unwrap()),
        )
        .unwrap();
        let draft = Database::open(context.paths.database())
            .unwrap()
            .context_draft(&context_id)
            .unwrap()
            .unwrap();
        assert!(
            String::from_utf8(draft.manifest)
                .unwrap()
                .contains("committed source")
        );
        context_preview(&context, &context_id).unwrap();
    }
}

#[test]
fn daemon_tick_reports_the_next_poll_interval() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let value = daemon_tick(&context).unwrap();
    assert_eq!(value["status"], "ok");
    assert_eq!(value["nextPollSeconds"], 30);
    assert!(value["channels"].as_array().unwrap().is_empty());
}

#[test]
fn daemon_tick_degrades_one_channel_without_skipping_the_rest() {
    let (directory, context) = home();
    init(&context).unwrap();

    let remote = directory.path().join("remote.git");
    bare_remote(&remote);

    let created = create(&context, remote.to_str().unwrap(), Some("OK")).unwrap();
    let channel_id = created["channelId"].as_str().unwrap().to_owned();

    let database = Database::open(context.paths.database()).unwrap();
    database
        .insert_channel(
            "bad-channel",
            "git",
            directory.path().join("missing.git").to_str().unwrap(),
            "Bad",
            "2026-09-13T00:00:00Z",
        )
        .unwrap();

    let value = daemon_tick(&context).unwrap();
    assert_eq!(value["status"], "degraded");
    let channels = value["channels"].as_array().unwrap();
    assert_eq!(channels.len(), 2);
    assert!(
        channels
            .iter()
            .any(|channel| channel["channelId"] == channel_id && channel["status"].is_null())
    );
    assert!(
        channels
            .iter()
            .any(|channel| channel["channelId"] == "bad-channel" && channel["status"] == "error")
    );
}

#[test]
fn the_provenance_banner_names_the_channel_a_message_came_from() {
    // Decision R1 of docs/REFACTOR.md. Until this test, nothing constructed
    // a `DaemonBroker`, which is how the banner on every approved delivery
    // came to say `local channel` whatever channel the body arrived on.
    let (_directory, context, _channel_id, principal) = messaging_home();
    let sent = send(&context, &principal, "which channel was this?", None).unwrap();
    sync_once(&context).unwrap();

    let broker = DaemonBroker::new(&context).unwrap();
    let message_id = sent["messageId"].as_str().unwrap();

    assert!(
        broker.pending(message_id).is_some(),
        "the message should be held for a decision"
    );
    assert_eq!(broker.channel_local_name(message_id), "Test channel");
    assert_eq!(
        broker.channel_local_name("01ARZ3NDEKTSV4RRFFQ69G5FAV"),
        "an unknown channel",
        "a message the broker does not hold is not guessed at"
    );
}

#[test]
fn the_daemon_tells_an_agent_its_principal_rather_than_its_device_key() {
    // docs/REFACTOR.md R4. The agent-safe `whoami` answered with the device's
    // signing key under the principal's name.
    let (_directory, context, _channel_id, principal) = messaging_home();

    let broker = DaemonBroker::new(&context).unwrap();
    let (principal_id, _device_id) = broker.local_identity();

    assert_eq!(principal_id, principal);
}
