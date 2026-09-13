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

    let value = doctor(&context).unwrap();
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

    let value = doctor(&context).unwrap();

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
fn doctor_finds_the_git_executable() {
    let (_directory, context) = home();
    init(&context).unwrap();

    let value = doctor(&context).unwrap();
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
    let sent = send(&context, &recipient, "must wait for the predecessor").unwrap();
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
