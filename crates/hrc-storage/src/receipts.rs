//! Delivery and read receipts (PRD section 18.3).

use super::*;

/// One receipt, as recorded locally.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedReceipt {
    /// The message reported on.
    pub message_id: String,
    /// Verified reporting principal.
    pub reporter_principal: String,
    /// Verified reporting device.
    pub reporter_device: String,
    /// The state reported.
    pub state: String,
    /// Why, when the state is a rejection.
    pub rejection_code: Option<String>,
    /// The reporter's timestamp.
    pub reported_at: String,
}

impl Database {
    /// Records one device's report about one message.
    ///
    /// Returns whether this was new. A repeat of a state a device already
    /// reported is ordinary: receipts ride the same at-least-once transport
    /// as everything else, so the same report can arrive twice.
    pub fn record_receipt(
        &self,
        channel_id: &str,
        receipt: &RecordedReceipt,
        now: &str,
    ) -> Result<bool> {
        let inserted = self.connection.execute(
            "INSERT INTO delivery_receipt (
                 message_id, channel_id, reporter_principal, reporter_device,
                 state, rejection_code, reported_at, recorded_at
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
             ON CONFLICT (message_id, reporter_device, state) DO NOTHING",
            params![
                receipt.message_id,
                channel_id,
                receipt.reporter_principal,
                receipt.reporter_device,
                receipt.state,
                receipt.rejection_code,
                receipt.reported_at,
                now,
            ],
        )?;

        Ok(inserted == 1)
    }

    /// Every receipt recorded for one message, in local recording order.
    pub fn receipts_for(&self, message_id: &str) -> Result<Vec<RecordedReceipt>> {
        let mut statement = self.connection.prepare(
            "SELECT message_id, reporter_principal, reporter_device, state,
                    rejection_code, reported_at
             FROM delivery_receipt
             WHERE message_id = ?1
             ORDER BY recorded_at, reporter_device, state",
        )?;

        let rows = statement.query_map(params![message_id], |row| {
            Ok(RecordedReceipt {
                message_id: row.get(0)?,
                reporter_principal: row.get(1)?,
                reporter_device: row.get(2)?,
                state: row.get(3)?,
                rejection_code: row.get(4)?,
                reported_at: row.get(5)?,
            })
        })?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    /// The devices that have reported a given state for a message.
    pub fn devices_reporting(&self, message_id: &str, state: &str) -> Result<Vec<String>> {
        let mut statement = self.connection.prepare(
            "SELECT reporter_device FROM delivery_receipt
             WHERE message_id = ?1 AND state = ?2
             ORDER BY reporter_device",
        )?;
        let rows =
            statement.query_map(params![message_id, state], |row| row.get::<_, String>(0))?;

        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }
}
