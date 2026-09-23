//! The append-only decision record and the audit log.

use super::*;

/// One approval decision, as the audit log preserves it.
///
/// Carries content rather than only digests. The question asked of an audit
/// trail later is not "was this approved" but "what was approved, and what
/// did the human change before it reached an agent" — and a digest cannot
/// answer the second half.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DecisionRecord {
    /// The channel it belongs to.
    pub channel_id: String,
    /// The message decided on.
    pub message_id: String,
    /// What was decided, for example `deliver_to_agent`.
    pub action: String,
    /// The content as it arrived.
    pub original_content: String,
    /// What was delivered instead, when the human edited it.
    pub edited_content: Option<String>,
    /// Digest of the original.
    pub content_hash: String,
    /// Digest of the edit.
    pub edited_hash: Option<String>,
    /// The local agent the human chose, if the decision reached one.
    pub agent: Option<String>,
    /// The local human who decided.
    pub decided_by: String,
    /// When.
    pub occurred_at: String,
}

/// One recorded audit entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditEntry {
    /// Channel the action applied to, when any.
    pub channel_id: Option<String>,
    /// Message the action applied to, when any.
    pub message_id: Option<String>,
    /// Stable action name.
    pub action: String,
    /// Optional detail for a human.
    pub detail: Option<String>,
    /// RFC 3339 time the action occurred.
    pub occurred_at: String,
}

impl Database {
    /// Appends one approval decision.
    ///
    /// There is no update or delete path for this table, here or anywhere
    /// else in the storage API. A decision that could be revised afterwards
    /// would not be evidence of anything.
    pub fn append_decision(&self, decision: &DecisionRecord) -> Result<()> {
        self.connection.execute(
            "INSERT INTO audit (
                 channel_id, message_id, action, content_hash, edited_hash,
                 original_content, edited_content, agent, decided_by, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                decision.channel_id,
                decision.message_id,
                decision.action,
                decision.content_hash,
                decision.edited_hash,
                decision.original_content.as_bytes(),
                decision.edited_content.as_deref().map(str::as_bytes),
                decision.agent,
                decision.decided_by,
                decision.occurred_at,
            ],
        )?;

        Ok(())
    }

    /// Commits a prompt-gate decision and the inbox/context lifecycle change
    /// as one transaction. Context has no separate disposition, so the
    /// foreign key makes this state transition govern it too.
    pub fn commit_decision(&mut self, decision: &DecisionRecord) -> Result<()> {
        let disposition = match decision.action.as_str() {
            "deliver_to_agent" => "approved",
            "deliver_edited" => "edited",
            "decline" => "declined",
            // An inbox-only decision intentionally leaves the message (and
            // its context) behind the trusted quarantine boundary.
            "keep_in_inbox" => "quarantined",
            _ => {
                return Err(StorageError::InvalidDecisionAction {
                    action: decision.action.clone(),
                });
            }
        };
        let transaction = self.connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE inbox SET disposition = ?3
             WHERE channel_id = ?1 AND message_id = ?2 AND disposition = 'quarantined'",
            params![decision.channel_id, decision.message_id, disposition],
        )?;
        if updated != 1 {
            return Err(StorageError::UnknownMessage {
                message_id: decision.message_id.clone(),
            });
        }
        transaction.execute(
            "INSERT INTO audit (
                 channel_id, message_id, action, content_hash, edited_hash,
                 original_content, edited_content, agent, decided_by, occurred_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                decision.channel_id,
                decision.message_id,
                decision.action,
                decision.content_hash,
                decision.edited_hash,
                decision.original_content.as_bytes(),
                decision.edited_content.as_deref().map(str::as_bytes),
                decision.agent,
                decision.decided_by,
                decision.occurred_at,
            ],
        )?;
        transaction.commit()?;
        Ok(())
    }

    /// Every decision recorded about one message, oldest first.
    pub fn decisions_for(&self, message_id: &str) -> Result<Vec<DecisionRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, message_id, action, content_hash, edited_hash,
                    original_content, edited_content, agent, decided_by, occurred_at
             FROM audit
             WHERE message_id = ?1 AND decided_by IS NOT NULL
             ORDER BY id",
        )?;

        let rows = statement.query_map(params![message_id], |row| {
            Ok(DecisionRecord {
                channel_id: row.get::<_, Option<String>>(0)?.unwrap_or_default(),
                message_id: row.get::<_, Option<String>>(1)?.unwrap_or_default(),
                action: row.get(2)?,
                content_hash: row.get::<_, Option<String>>(3)?.unwrap_or_default(),
                edited_hash: row.get(4)?,
                original_content: row
                    .get::<_, Option<Vec<u8>>>(5)?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
                    .unwrap_or_default(),
                edited_content: row
                    .get::<_, Option<Vec<u8>>>(6)?
                    .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
                agent: row.get(7)?,
                decided_by: row.get::<_, Option<String>>(8)?.unwrap_or_default(),
                occurred_at: row.get(9)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Appends an audit record. There is no update or delete counterpart.
    pub fn append_audit(
        &self,
        channel_id: Option<&str>,
        message_id: Option<&str>,
        action: &str,
        content_hash: Option<&str>,
        detail: Option<&str>,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO audit (channel_id, message_id, action, content_hash, detail, occurred_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![channel_id, message_id, action, content_hash, detail, now],
        )?;
        Ok(())
    }

    /// Audit actions in the order they occurred.
    pub fn audit_actions(&self) -> Result<Vec<String>> {
        let mut statement = self
            .connection
            .prepare("SELECT action FROM audit ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Audit entries in reverse chronological order.
    pub fn audit_entries(&self, since: Option<&str>) -> Result<Vec<AuditEntry>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, message_id, action, detail, occurred_at
             FROM audit
             WHERE (?1 IS NULL OR occurred_at >= ?1)
             ORDER BY occurred_at DESC, id DESC",
        )?;
        let rows = statement.query_map(params![since], |row| {
            Ok(AuditEntry {
                channel_id: row.get(0)?,
                message_id: row.get(1)?,
                action: row.get(2)?,
                detail: row.get(3)?,
                occurred_at: row.get(4)?,
            })
        })?;
        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }
}
