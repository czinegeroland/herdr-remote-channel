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

    let mut record = |name: &'static str, ok: bool, detail: String| {
        checks.push(Check { name, ok, detail });
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

    Ok(Diagnosis { checks })
}

/// One thing `hrc doctor` verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    /// What was checked, as the report names it.
    pub name: &'static str,
    /// Whether it passed.
    pub ok: bool,
    /// What was found.
    pub detail: String,
}

/// What `hrc doctor` found.
///
/// Typed, so that the exit code and the daemon's health summary are computed
/// from the checks themselves rather than read back out of the printed
/// report, where a renamed JSON key would silently change them
/// (`docs/REFACTOR.md` S2 and R9).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnosis {
    /// Every check, in the order it ran.
    pub checks: Vec<Check>,
}

impl Diagnosis {
    /// Whether every check passed.
    pub fn healthy(&self) -> bool {
        self.checks.iter().all(|check| check.ok)
    }

    /// The report `hrc doctor` prints.
    pub fn report(&self) -> Value {
        let healthy = self.healthy();
        let checks: Vec<Value> = self
            .checks
            .iter()
            .map(|check| json!({ "check": check.name, "ok": check.ok, "detail": check.detail }))
            .collect();

        json!({
            "status": if healthy { "ok" } else { "unhealthy" },
            "healthy": healthy,
            "checks": checks,
        })
    }
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
