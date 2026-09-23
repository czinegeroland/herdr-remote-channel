//! Channels: their records, cursors, counts, and the sticky halt.

use super::*;

/// A channel's locally tracked state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChannelRecord {
    /// Channel identifier.
    pub channel_id: String,
    /// Transport kind, for example `git`.
    pub transport_kind: String,
    /// Transport locator.
    pub transport_locator: String,
    /// Local display name. Never sender-controlled.
    pub local_name: String,
    /// Roster epoch evaluated so far.
    pub roster_epoch: u64,
    /// Control sequence applied so far.
    pub control_sequence: u64,
    /// Last fully processed transport revision.
    pub sync_cursor: Option<String>,
    /// Last transport revision whose message bodies were received locally.
    pub receive_cursor: Option<String>,
    /// Why synchronization is halted, when it is.
    pub halted_reason: Option<String>,
}

/// Counts reported by `hrc status` for one channel.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChannelCounts {
    /// Messages awaiting publication.
    pub outbox_pending: u64,
    /// Inbox entries not yet read.
    pub unread: u64,
    /// Inbox entries awaiting a local approval decision.
    pub pending_approval: u64,
    /// The most recent transport error recorded against this channel.
    pub last_error: Option<String>,
}

impl Database {
    /// Registers a channel, or returns an error if it already exists.
    pub fn insert_channel(
        &self,
        channel_id: &str,
        transport_kind: &str,
        transport_locator: &str,
        local_name: &str,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO channel (
                 channel_id, transport_kind, transport_locator, local_name, created_at
             ) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                channel_id,
                transport_kind,
                transport_locator,
                local_name,
                now
            ],
        )?;
        Ok(())
    }

    /// Reads a channel's state.
    pub fn channel(&self, channel_id: &str) -> Result<Option<ChannelRecord>> {
        self.connection
            .query_row(
                "SELECT channel_id, transport_kind, transport_locator, local_name,
                        roster_epoch, control_sequence, sync_cursor, receive_cursor,
                        halted_reason
                 FROM channel WHERE channel_id = ?1",
                params![channel_id],
                |row| {
                    Ok(ChannelRecord {
                        channel_id: row.get(0)?,
                        transport_kind: row.get(1)?,
                        transport_locator: row.get(2)?,
                        local_name: row.get(3)?,
                        roster_epoch: row.get::<_, i64>(4)? as u64,
                        control_sequence: row.get::<_, i64>(5)? as u64,
                        sync_cursor: row.get(6)?,
                        receive_cursor: row.get(7)?,
                        halted_reason: row.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(StorageError::from)
    }

    /// Records progress after a control chain has been evaluated.
    pub fn set_roster_progress(
        &self,
        channel_id: &str,
        roster_epoch: u64,
        control_sequence: u64,
    ) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET roster_epoch = ?2, control_sequence = ?3 WHERE channel_id = ?1",
            params![channel_id, roster_epoch as i64, control_sequence as i64],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Advances the durable synchronization cursor.
    pub fn set_sync_cursor(&self, channel_id: &str, cursor: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET sync_cursor = ?2 WHERE channel_id = ?1",
            params![channel_id, cursor],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Advances the durable cursor for locally received message bodies.
    ///
    /// This is deliberately independent of the transport cursor: locked
    /// synchronization may validate and cache transport history without
    /// claiming that encrypted message bodies were processed.
    pub fn set_receive_cursor(&self, channel_id: &str, cursor: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET receive_cursor = ?2 WHERE channel_id = ?1",
            params![channel_id, cursor],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Halts synchronization for a channel with a sticky reason.
    ///
    /// The reason is never overwritten by a later halt: the first detected
    /// cause is the one worth showing, and a cascade of downstream failures
    /// should not bury it.
    pub fn halt_channel(&self, channel_id: &str, reason: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE channel SET halted_reason = COALESCE(halted_reason, ?2)
             WHERE channel_id = ?1",
            params![channel_id, reason],
        )?;
        Self::expect_one(updated, channel_id)
    }

    /// Every registered channel, in insertion order.
    pub fn channels(&self) -> Result<Vec<ChannelRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT channel_id, transport_kind, transport_locator, local_name,
                    roster_epoch, control_sequence, sync_cursor, receive_cursor,
                    halted_reason
             FROM channel ORDER BY created_at, channel_id",
        )?;

        let rows = statement.query_map([], |row| {
            Ok(ChannelRecord {
                channel_id: row.get(0)?,
                transport_kind: row.get(1)?,
                transport_locator: row.get(2)?,
                local_name: row.get(3)?,
                roster_epoch: row.get::<_, i64>(4)? as u64,
                control_sequence: row.get::<_, i64>(5)? as u64,
                sync_cursor: row.get(6)?,
                receive_cursor: row.get(7)?,
                halted_reason: row.get(8)?,
            })
        })?;

        Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
    }

    /// Counts that `hrc status` reports for one channel (PRD section 29).
    pub fn channel_counts(&self, channel_id: &str) -> Result<ChannelCounts> {
        let outbox_pending = self.connection.query_row(
            "SELECT COUNT(*) FROM outbox
             WHERE channel_id = ?1 AND state IN ('reserved', 'queued', 'publishing')",
            params![channel_id],
            |row| row.get::<_, i64>(0),
        )?;

        let unread = self.connection.query_row(
            "SELECT COUNT(*) FROM inbox WHERE channel_id = ?1 AND read_at IS NULL",
            params![channel_id],
            |row| row.get::<_, i64>(0),
        )?;

        let pending_approval = self.connection.query_row(
            "SELECT COUNT(*) FROM inbox
             WHERE channel_id = ?1
               AND disposition = 'quarantined'
               AND arrival_sequence IS NOT NULL",
            params![channel_id],
            |row| row.get::<_, i64>(0),
        )?;

        let last_error = self
            .connection
            .query_row(
                "SELECT last_error FROM outbox
                 WHERE channel_id = ?1 AND last_error IS NOT NULL
                 ORDER BY updated_at DESC LIMIT 1",
                params![channel_id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten();

        Ok(ChannelCounts {
            outbox_pending: outbox_pending as u64,
            unread: unread as u64,
            pending_approval: pending_approval as u64,
            last_error,
        })
    }
}
