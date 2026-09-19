//! The plugin inbox view (PRD section 23.2).
//!
//! Section 23.2 lists what a row must show. Every one of those fields is
//! derived from [`AgentView`], which is the closed metadata set of section
//! 19.1 — so the inbox cannot display sender-chosen text even by accident,
//! because it never receives any. Attachment *names* and subjects stay
//! quarantined with the body; a count and a byte total do not.
//!
//! The row carries no body and no field that could hold one. Showing a
//! decrypted body is the trusted approval screen's job (`hrc review`), and
//! keeping the two apart is what stops a plugin pane from becoming an
//! agent-readable surface for pending content.

use hrc_core::gate::AgentView;
use hrc_storage::PluginInboxEntry;
use serde::Serialize;

/// Whether the local checks on a message passed.
///
/// A message only reaches the inbox after its signature verified and its
/// sender resolved in the roster, so a row cannot be *unverified* — the
/// alternatives are states the message reached afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verification {
    /// Signature and sender verified, and the message is still valid.
    Verified,
    /// Verified when it arrived, but its stated expiry has passed.
    ///
    /// Section 26 requires an expired message to be displayed as expired and
    /// to allow no action.
    Expired,
}

/// The result of scanning content for secrets.
///
/// HRC scans *outgoing* content (requirement HRC-SEC-008). An inbound
/// message is not scanned, because a finding in someone else's message is
/// not a local disclosure and suppressing the row would hide it from the
/// person whose judgment the gate exists to invoke. Saying so explicitly
/// beats rendering a reassuring blank.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SecretScan {
    /// Scanned before sending, with no findings.
    Clean,
    /// Scanned before sending, and something was found.
    Findings,
    /// Inbound content, which this installation does not scan.
    NotApplicable,
}

/// What the human may do with a row right now (PRD section 19.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalDecision {
    /// Deliver the original content to a chosen local agent.
    DeliverToAgent,
    /// Edit or redact before delivery.
    DeliverEdited,
    /// Leave it in the human inbox.
    KeepInInbox,
    /// Decline, with or without a reason.
    Decline,
}

/// Where a message has got to locally (PRD section 19.2).
///
/// Derived from the stored disposition rather than tracked alongside it, so
/// the side view cannot disagree with the prompt gate about what happened to
/// a message.
///
/// There is deliberately no `Kept` variant. Keeping a message in the human
/// inbox leaves it `quarantined` in storage — `Database::commit_decision`
/// maps `keep_in_inbox` back to the state it was already in, because a kept
/// message is still one a person may decide about later. A `Kept` row would
/// therefore be the UI asserting something storage does not record, which is
/// the second source of truth this enum exists to avoid (decision DEC-088).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxDisposition {
    /// Still quarantined and still awaiting a human decision.
    Pending,
    /// Approved and handed to a local agent, as sent or edited.
    Delivered,
    /// Refused.
    Declined,
    /// Past its stated expiry; no action remains (section 26).
    Expired,
    /// A kind this build does not understand.
    Unsupported,
}

impl InboxDisposition {
    /// Reads the stored disposition string.
    ///
    /// An unrecognized value maps to [`InboxDisposition::Unsupported`]
    /// rather than panicking or defaulting to `Pending`: a row this build
    /// cannot classify is one it must not offer a decision on.
    pub fn from_stored(disposition: &str) -> Self {
        match disposition {
            "quarantined" => InboxDisposition::Pending,
            "approved" | "edited" => InboxDisposition::Delivered,
            "declined" => InboxDisposition::Declined,
            "expired" => InboxDisposition::Expired,
            _ => InboxDisposition::Unsupported,
        }
    }

    /// The fixed local word a row shows.
    pub const fn as_str(self) -> &'static str {
        match self {
            InboxDisposition::Pending => "PENDING",
            InboxDisposition::Delivered => "DELIVERED",
            InboxDisposition::Declined => "DECLINED",
            InboxDisposition::Expired => "EXPIRED",
            InboxDisposition::Unsupported => "UNSUPPORTED",
        }
    }
}

/// One inbox line.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxRow {
    /// The stored identifier this row is selected and targeted by.
    ///
    /// Never rendered. Use [`InboxRow::message_label`] for that.
    pub message_id: String,
    /// The validated identifier a surface may print.
    pub message_label: String,
    /// Verified sender principal, resolved locally from the roster.
    pub sender_principal: String,
    /// Locally chosen display name for that principal.
    pub sender_local_name: String,
    /// Enumerated message kind, or `unsupported`.
    pub kind: String,
    /// Validated thread identifier, or a fixed local label.
    pub thread_label: String,
    /// Locally resolved channel name.
    pub channel_local_name: String,
    /// When this installation first held the message.
    pub arrival_at: String,
    /// Stated expiry, when the message has one.
    pub expires_at: Option<String>,
    /// Validated endpoint identifier, or a fixed local label.
    pub endpoint_label: String,
    /// Whether the sender is requesting the prompt capability.
    pub prompt_request: bool,
    /// How many attachments the envelope declares.
    pub attachment_count: u32,
    /// Total declared size of those attachments.
    pub attachment_bytes: u64,
    /// Whether the local checks passed.
    pub verification: Verification,
    /// Whether the content was secret scanned, and what was found.
    pub secret_scan: SecretScan,
    /// Where the message has got to.
    pub disposition: InboxDisposition,
    /// What the human may do with it now.
    pub decisions: Vec<LocalDecision>,
}

impl InboxRow {
    /// Builds a row from the agent-safe view of a quarantined message.
    ///
    /// `now` is the current RFC 3339 UTC time, compared lexicographically
    /// against the stated expiry. RFC 3339 UTC timestamps with the same
    /// fixed shape order correctly as strings, which is what the rest of the
    /// codebase relies on, and it avoids parsing a sender-supplied timestamp
    /// into arithmetic that could disagree with the check the gate already
    /// makes when the body is opened.
    pub fn from_view(view: &AgentView, message_id: &str, now: &str) -> Self {
        let expired = view
            .expires_at
            .as_deref()
            .is_some_and(|expires_at| expires_at <= now);

        let verification = if expired {
            Verification::Expired
        } else {
            Verification::Verified
        };

        // An expired message allows no action (section 26), and one already
        // decided is not awaiting a decision. Both produce an empty list
        // rather than a list the screen has to remember to disable.
        let decisions = if expired || !view.awaiting_decision {
            Vec::new()
        } else {
            vec![
                LocalDecision::DeliverToAgent,
                LocalDecision::DeliverEdited,
                LocalDecision::KeepInInbox,
                LocalDecision::Decline,
            ]
        };

        // Derived from the view rather than from a stored string, because
        // `AgentView` records only whether a decision is still outstanding.
        // The precise state of a decided message comes from storage, and
        // `from_entry` overwrites this with it.
        let disposition = if expired {
            InboxDisposition::Expired
        } else if view.awaiting_decision {
            InboxDisposition::Pending
        } else {
            InboxDisposition::Delivered
        };

        Self {
            message_id: message_id.to_owned(),
            message_label: hrc_core::gate::message_label(message_id),
            sender_principal: view.sender_principal.clone(),
            sender_local_name: view.sender_local_name.clone(),
            kind: view.kind.clone(),
            thread_label: view.thread_label.clone(),
            channel_local_name: view.channel_local_name.clone(),
            arrival_at: view.arrival_at.clone(),
            expires_at: view.expires_at.clone(),
            endpoint_label: view.endpoint_label.clone(),
            prompt_request: view.prompt_request,
            attachment_count: view.attachment_count,
            attachment_bytes: view.attachment_bytes,
            verification,
            secret_scan: SecretScan::NotApplicable,
            disposition,
            decisions,
        }
    }

    /// Builds a row from stored inbox metadata.
    ///
    /// The second path into a row, and the one the short-lived plugin
    /// process actually takes: it reads the database directly rather than
    /// asking a daemon that may not be running. It validates the stored
    /// endpoint and thread through the same `hrc-core` helpers that
    /// [`hrc_core::gate::agent_view`] uses, so the two cannot disagree about
    /// what a valid label looks like.
    pub fn from_entry(
        entry: &PluginInboxEntry,
        sender_local_name: &str,
        channel_local_name: &str,
        now: &str,
    ) -> Self {
        let mut row = Self::from_view(
            &agent_view(entry, sender_local_name, channel_local_name),
            &entry.message_id,
            now,
        );

        // Storage knows which decided state this is; the agent-safe view only
        // knows that it is decided. An expiry that has passed still wins, so
        // that a row stored as quarantined but out of time reads as expired
        // rather than as awaiting a decision it can no longer receive.
        if row.disposition != InboxDisposition::Expired {
            row.disposition = InboxDisposition::from_stored(&entry.disposition);
        }

        row
    }

    /// Whether the human still has a decision to make about this row.
    pub fn awaiting_decision(&self) -> bool {
        !self.decisions.is_empty()
    }
}

/// The section 19.1 metadata set for one stored inbox row.
///
/// Extracted from [`InboxRow::from_entry`] so the daemon's agent-safe
/// surface and the short-lived plugin process build the same view from the
/// same row. Two independent mappings from storage to the closed metadata
/// set would be two places for a body-adjacent field to appear in one and
/// not the other.
pub fn agent_view(
    entry: &PluginInboxEntry,
    sender_local_name: &str,
    channel_local_name: &str,
) -> AgentView {
    let kind = match hrc_protocol::MessageKind::parse(&entry.kind) {
        Some(kind) => kind.as_str().to_owned(),
        None => "unsupported".to_owned(),
    };

    AgentView {
        sender_principal: entry.sender_principal.clone(),
        sender_local_name: sender_local_name.to_owned(),
        kind,
        channel_local_name: channel_local_name.to_owned(),
        endpoint_label: hrc_core::gate::endpoint_label(entry.endpoint.as_deref()),
        ciphertext_bytes: entry.ciphertext_bytes,
        plaintext_bytes: entry.plaintext_bytes,
        created_at: entry.created_at.clone(),
        arrival_at: entry.received_at.clone(),
        expires_at: entry.expires_at.clone(),
        thread_label: hrc_core::gate::thread_label(entry.thread_id.as_deref()),
        prompt_request: entry.prompt_request,
        attachment_count: entry.attachment_count,
        attachment_bytes: entry.attachment_bytes,
        awaiting_decision: entry.disposition == "quarantined",
    }
}

/// The whole inbox, as the plugin renders it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InboxView {
    /// The rows, most recently arrived last.
    pub rows: Vec<InboxRow>,
}

impl InboxView {
    /// Builds the view over rows, most recently arrived last.
    ///
    /// Takes rows rather than [`AgentView`]s because a row is now selected by
    /// its message identifier, which the agent-safe view does not carry: the
    /// identifier is a local key, not part of the section 19.1 metadata set.
    /// Ordering lives here so that every caller sorts the same way.
    pub fn new(mut rows: Vec<InboxRow>) -> Self {
        rows.sort_by(|left, right| {
            left.arrival_at
                .cmp(&right.arrival_at)
                .then_with(|| left.message_id.cmp(&right.message_id))
        });

        Self { rows }
    }

    /// How many rows still await a human decision.
    pub fn pending(&self) -> usize {
        self.rows
            .iter()
            .filter(|row| row.awaiting_decision())
            .count()
    }
}

#[cfg(test)]
mod tests;
