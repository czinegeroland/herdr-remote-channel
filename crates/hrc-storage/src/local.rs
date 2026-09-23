//! State that never leaves this installation: the notification ledger and
//! locally assigned names.

use super::*;

impl Database {
    /// Records that a notification was raised, and says whether it is new.
    ///
    /// Returns `true` the first time a message-and-kind pair is seen and
    /// `false` every time after, including after a restart — which is the
    /// whole point. Deduplication held in a process that lives for one pane
    /// render is no deduplication at all.
    ///
    /// One statement rather than a read and a write, so two panes refreshing
    /// at the same moment cannot both decide they are the first.
    /// `ON CONFLICT DO NOTHING` makes the insert the test: the row is
    /// created and the notification is new, or the row was already there and
    /// it is not.
    pub fn record_notification(
        &self,
        channel_id: &str,
        message_id: &str,
        kind: &str,
        now: &str,
    ) -> Result<bool> {
        let inserted = self.connection.execute(
            "INSERT INTO notification (message_id, kind, channel_id, notified_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (message_id, kind) DO NOTHING",
            params![message_id, kind, channel_id, now],
        )?;

        Ok(inserted == 1)
    }

    /// Marks every notification about a message as no longer outstanding.
    ///
    /// Called when a message is decided or expires. The row stays, carrying
    /// the time it was resolved: a deleted row and a row that was never
    /// written are indistinguishable, and telling them apart is what stops a
    /// decided message being announced again.
    pub fn resolve_notifications(&self, message_id: &str, now: &str) -> Result<usize> {
        Ok(self.connection.execute(
            "UPDATE notification SET resolved_at = ?2
             WHERE message_id = ?1 AND resolved_at IS NULL",
            params![message_id, now],
        )?)
    }

    /// The notifications raised about a channel and not yet resolved.
    ///
    /// Returns `(message_id, kind)` pairs, oldest first.
    pub fn outstanding_notifications(&self, channel_id: &str) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, kind FROM notification
             WHERE channel_id = ?1 AND resolved_at IS NULL
             ORDER BY notified_at, message_id",
        )?;
        let rows =
            statement.query_map(params![channel_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Records the local human's chosen display name for a principal.
    ///
    /// Replaces any previous name rather than accumulating them: this is
    /// what the local person calls someone *now*, not a history of what they
    /// have called them. The audit log is where the sequence of decisions
    /// belongs, and a name is not a decision about a message.
    ///
    /// Reaching this method is a trusted operation. An alias is the text a
    /// human reads when deciding whether to trust a message, so an agent
    /// that could write one could relabel a stranger as a colleague --
    /// which would defeat the gate by way of the only field the gate's own
    /// screen asks the human to recognize.
    pub fn set_principal_alias(
        &self,
        channel_id: &str,
        principal: &str,
        display_name: &str,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO principal_alias (channel_id, principal, display_name, assigned_at)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT (channel_id, principal)
             DO UPDATE SET display_name = excluded.display_name,
                           assigned_at = excluded.assigned_at",
            params![channel_id, principal, display_name, now],
        )?;

        Ok(())
    }

    /// Forgets the local display name for a principal.
    ///
    /// Deleting rather than blanking, because an empty name and no name are
    /// the same thing to every surface that reads this table, and one of the
    /// two would be a value the render code has to remember to reject.
    pub fn clear_principal_alias(&self, channel_id: &str, principal: &str) -> Result<usize> {
        Ok(self.connection.execute(
            "DELETE FROM principal_alias WHERE channel_id = ?1 AND principal = ?2",
            params![channel_id, principal],
        )?)
    }

    /// Every locally assigned display name in a channel, by principal.
    ///
    /// Returned whole rather than looked up per row: the inbox resolves one
    /// name per line and a channel has a handful of members, so one query
    /// beats one query per message, and every row in a single render is then
    /// resolved against the same snapshot.
    pub fn principal_aliases(&self, channel_id: &str) -> Result<BTreeMap<String, String>> {
        let mut statement = self
            .connection
            .prepare("SELECT principal, display_name FROM principal_alias WHERE channel_id = ?1")?;
        let rows = statement.query_map(params![channel_id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        rows.collect::<std::result::Result<BTreeMap<_, _>, _>>()
            .map_err(Into::into)
    }
}
