//! Invites this installation issued.

use super::*;

/// One invite this installation issued.
///
/// The secret is absent by construction: it is handed to the invited person
/// and never stored, because an invite secret at rest is a second copy of
/// something that only ever needed to exist once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteRecord {
    /// The invite's published identifier.
    pub invite_id: String,
    /// Who the administrator meant it for, in their own words.
    pub intended_for: String,
    /// When it lapses.
    pub expires_at: String,
    /// `open`, `consumed`, or `revoked`.
    pub state: String,
}

impl Database {
    /// Records an invite this installation issued.
    pub fn record_invite(
        &self,
        channel_id: &str,
        invite_id: &str,
        intended_for: &str,
        expires_at: &str,
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO invite (invite_id, channel_id, intended_for, expires_at, created_at, state)
             VALUES (?1, ?2, ?3, ?4, ?5, 'open')",
            params![invite_id, channel_id, intended_for, expires_at, now],
        )?;

        Ok(())
    }

    /// Invites issued for a channel, newest first.
    pub fn invites(&self, channel_id: &str) -> Result<Vec<InviteRecord>> {
        let mut statement = self.connection.prepare(
            "SELECT invite_id, intended_for, expires_at, state FROM invite
             WHERE channel_id = ?1
             ORDER BY created_at DESC, invite_id",
        )?;

        let rows = statement.query_map(params![channel_id], |row| {
            Ok(InviteRecord {
                invite_id: row.get(0)?,
                intended_for: row.get(1)?,
                expires_at: row.get(2)?,
                state: row.get(3)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// Updates an invite's locally recorded state.
    pub fn set_invite_state(&self, channel_id: &str, invite_id: &str, state: &str) -> Result<()> {
        let updated = self.connection.execute(
            "UPDATE invite SET state = ?3 WHERE channel_id = ?1 AND invite_id = ?2",
            params![channel_id, invite_id, state],
        )?;

        if updated == 0 {
            return Err(StorageError::UnknownInvite {
                invite_id: invite_id.to_owned(),
            });
        }

        Ok(())
    }
}
