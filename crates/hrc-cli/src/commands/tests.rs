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
