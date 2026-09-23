//! `hrc status`, `hrc doctor` and `hrc audit`.

use super::*;

/// `hrc status`: report the fields PRD section 29 requires.
pub fn status(context: &Context) -> Result<Value> {
    let paths = &context.paths;
    let database = Database::open(paths.database())?;

    let mut channels = Vec::new();
    for channel in database.channels()? {
        let counts = database.channel_counts(&channel.channel_id)?;

        channels.push(json!({
            "channelId": channel.channel_id,
            "localName": channel.local_name,
            "transport": channel.transport_kind,
            "locator": channel.transport_locator,
            "rosterEpoch": channel.roster_epoch,
            "controlSequence": channel.control_sequence,
            "lastFetchedRevision": channel.sync_cursor,
            "outboxPending": counts.outbox_pending,
            "unread": counts.unread,
            "pendingApproval": counts.pending_approval,
            "lastTransportError": counts.last_error,
            "haltedReason": channel.halted_reason,
        }));
    }

    Ok(json!({
        "status": "ok",
        "home": paths.home().display().to_string(),
        "channels": channels,
    }))
}

/// `hrc doctor`: verify the local installation.
///
/// Every check reports its own result rather than the command stopping at
/// the first failure: a user diagnosing a broken installation wants the
/// whole picture, not one symptom at a time.
pub fn diagnose(context: &Context) -> Result<Diagnosis> {
    let paths = &context.paths;
    let mut checks = Vec::new();
    let mut healthy = true;

    let mut record = |name: &str, ok: bool, detail: String| {
        healthy &= ok;
        checks.push(json!({ "check": name, "ok": ok, "detail": detail }));
    };

    record(
        "protocol_version",
        true,
        format!("protocol {}", hrc_protocol::PROTOCOL_VERSION),
    );

    match git_version() {
        Some(version) => record("git", true, version),
        None => record(
            "git",
            false,
            "the git executable was not found on PATH".into(),
        ),
    }

    let home_exists = paths.home().is_dir();
    record(
        "state_directory",
        home_exists,
        paths.home().display().to_string(),
    );

    match Database::open(paths.database()) {
        Ok(database) => match database.integrity_check() {
            Ok(true) => record("database", true, "integrity check passed".into()),
            Ok(false) => record("database", false, "integrity check failed".into()),
            Err(error) => record("database", false, error.to_string()),
        },
        Err(error) => record("database", false, error.to_string()),
    }

    match context.key_store() {
        Ok(store) => match store.contains(DEVICE_KEY_NAME) {
            Ok(true) => record("device_keys", true, "device keys are present".into()),
            Ok(false) => record(
                "device_keys",
                false,
                "no device keys; run `hrc init`".into(),
            ),
            Err(error) => record("device_keys", false, error.to_string()),
        },
        Err(error) => record("device_keys", false, error.to_string()),
    }

    Ok(Diagnosis {
        report: json!({
            "status": if healthy { "ok" } else { "unhealthy" },
            "healthy": healthy,
            "checks": checks,
        }),
        healthy,
    })
}

/// What `hrc doctor` found.
///
/// `healthy` is carried beside the report rather than read back out of it,
/// so the exit code cannot drift from the checks because a JSON key was
/// renamed (`docs/REFACTOR.md` S2).
pub struct Diagnosis {
    /// The report `hrc doctor` prints.
    pub report: Value,
    /// Whether every check passed.
    pub healthy: bool,
}

/// `hrc doctor`'s report, for callers that only want what it says.
pub fn doctor(context: &Context) -> Result<Value> {
    diagnose(context).map(|diagnosis| diagnosis.report)
}

/// `hrc audit`: show the local audit log.
pub fn audit(context: &Context, since: Option<&str>) -> Result<Value> {
    let database = Database::open(context.paths.database())?;

    let entries: Vec<Value> = database
        .audit_entries(since)?
        .into_iter()
        .map(|entry| {
            json!({
                "occurredAt": entry.occurred_at,
                "action": entry.action,
                "channelId": entry.channel_id,
                "messageId": entry.message_id,
                "detail": entry.detail,
            })
        })
        .collect();

    Ok(json!({
        "status": "ok",
        "entries": entries,
    }))
}

/// The installed Git version, if Git is on the path.
pub(super) fn git_version() -> Option<String> {
    let output = std::process::Command::new("git")
        .arg("--version")
        .output()
        .ok()?;

    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_owned())
}
