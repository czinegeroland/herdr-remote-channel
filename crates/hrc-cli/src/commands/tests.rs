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
    let mut publisher = peer(directory.path(), "publisher", &remote);
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
            1,
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
