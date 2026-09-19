//! What the plugin notifies about (PRD section 23.3).
//!
//! A closed set, for the same reason the agent-safe response enum is closed:
//! a notification is a string that reaches a human out of context, and the
//! shortest path to putting sender-chosen text on someone's screen is a
//! free-form notification body. Every variant below carries only locally
//! resolved names, validated identifiers, and counts.

use serde::Serialize;

/// One thing worth telling the human about.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Notification {
    /// A question arrived.
    NewQuestion {
        /// Locally chosen display name for the verified sender.
        sender_local_name: String,
        /// Locally resolved channel name.
        channel_local_name: String,
    },
    /// A prompt request arrived and is waiting behind the gate.
    NewPromptRequest {
        /// Locally chosen display name for the verified sender.
        sender_local_name: String,
        /// Locally resolved channel name.
        channel_local_name: String,
    },
    /// A task request arrived.
    NewTaskRequest {
        /// Locally chosen display name for the verified sender.
        sender_local_name: String,
        /// Locally resolved channel name.
        channel_local_name: String,
    },
    /// Someone is waiting to be admitted.
    JoinAwaitingApproval {
        /// Locally resolved channel name.
        channel_local_name: String,
        /// How many requests are waiting.
        waiting: usize,
    },
    /// An outbound message could not be delivered.
    FailedDelivery {
        /// Locally resolved channel name.
        channel_local_name: String,
    },
    /// The roster or a key changed.
    MembershipChanged {
        /// Locally resolved channel name.
        channel_local_name: String,
        /// The epoch the channel moved to.
        roster_epoch: u64,
    },
    /// Synchronization halted because the history was rewritten, an object
    /// was deleted, or one was substituted.
    ///
    /// Carries no reason text. The halt reason is recorded locally and shown
    /// on the trusted surface; a notification that quoted it would be
    /// quoting a string derived from what an attacker published.
    TamperDetected {
        /// Locally resolved channel name.
        channel_local_name: String,
    },
}

impl Notification {
    /// The notification an arriving message should raise, if any.
    ///
    /// Kinds outside section 23.3's list are not notified. A note is not
    /// urgent, and an unsupported kind is by definition something this
    /// version does not understand well enough to interrupt someone over.
    ///
    /// A prompt request is not a message kind: it is any message carrying
    /// the `prompt:request` capability, and it outranks the kind it arrived
    /// on because it is the one that blocks on a human.
    pub fn for_message(row: &crate::inbox::InboxRow) -> Option<Self> {
        let sender_local_name = row.sender_local_name.clone();
        let channel_local_name = row.channel_local_name.clone();

        if row.prompt_request {
            return Some(Notification::NewPromptRequest {
                sender_local_name,
                channel_local_name,
            });
        }

        match row.kind.as_str() {
            "question" => Some(Notification::NewQuestion {
                sender_local_name,
                channel_local_name,
            }),
            "task" => Some(Notification::NewTaskRequest {
                sender_local_name,
                channel_local_name,
            }),
            _ => None,
        }
    }

    /// The line the human sees.
    pub fn render(&self) -> String {
        match self {
            Notification::NewQuestion {
                sender_local_name,
                channel_local_name,
            } => format!("{sender_local_name} asked a question in {channel_local_name}"),
            Notification::NewPromptRequest {
                sender_local_name,
                channel_local_name,
            } => format!(
                "{sender_local_name} requested a prompt in {channel_local_name}; it is waiting for your approval"
            ),
            Notification::NewTaskRequest {
                sender_local_name,
                channel_local_name,
            } => format!("{sender_local_name} requested a task in {channel_local_name}"),
            Notification::JoinAwaitingApproval {
                channel_local_name,
                waiting,
            } => format!("{waiting} waiting to join {channel_local_name}"),
            Notification::FailedDelivery { channel_local_name } => {
                format!("delivery failed in {channel_local_name}")
            }
            Notification::MembershipChanged {
                channel_local_name,
                roster_epoch,
            } => format!("{channel_local_name} membership changed; now at epoch {roster_epoch}"),
            Notification::TamperDetected { channel_local_name } => format!(
                "{channel_local_name} HALTED: its published history was rewritten, deleted, or substituted"
            ),
        }
    }

    /// The closed tag this notification is recorded under.
    ///
    /// The key the durable ledger deduplicates on, alongside the message. It
    /// is a fixed word from this enum and never sender-chosen text, which is
    /// what makes it safe to store and to compare — and what keeps a notice
    /// about one message from suppressing a different notice about the same
    /// message.
    pub const fn kind(&self) -> &'static str {
        match self {
            Notification::NewQuestion { .. } => "new_question",
            Notification::NewPromptRequest { .. } => "new_prompt_request",
            Notification::NewTaskRequest { .. } => "new_task_request",
            Notification::JoinAwaitingApproval { .. } => "join_awaiting_approval",
            Notification::FailedDelivery { .. } => "failed_delivery",
            Notification::MembershipChanged { .. } => "membership_changed",
            Notification::TamperDetected { .. } => "tamper_detected",
        }
    }

    /// Whether this needs the human now rather than at their convenience.
    pub fn is_urgent(&self) -> bool {
        matches!(
            self,
            Notification::TamperDetected { .. }
                | Notification::NewPromptRequest { .. }
                | Notification::JoinAwaitingApproval { .. }
        )
    }
}

/// What to raise for a batch of newly actionable messages.
///
/// A burst of arrivals is one notice, not one per message. Seven toasts in a
/// row is the behaviour people turn notifications off over, and the thing
/// that matters — something is waiting — is said once.
///
/// Urgent notices are never collapsed. A tamper halt and a prompt request
/// each name a specific thing a person has to act on, and folding them into
/// "4 new remote requests" would lose exactly the information that made them
/// urgent. So urgent ones are returned as they are, and only the ordinary
/// ones coalesce.
///
/// The coalesced line contains a count and fixed local wording. There is no
/// path from a sender's text into it, because it is built from how many
/// there are rather than from what any of them said.
pub fn coalesce(pending: Vec<Notification>) -> Vec<Coalesced> {
    let (urgent, ordinary): (Vec<_>, Vec<_>) = pending
        .into_iter()
        .partition(|notification| notification.is_urgent());

    let mut raised: Vec<Coalesced> = urgent
        .into_iter()
        .map(|notification| Coalesced {
            text: notification.render(),
            urgent: true,
            covers: 1,
        })
        .collect();

    match ordinary.len() {
        0 => {}
        1 => {
            raised.push(Coalesced {
                text: ordinary[0].render(),
                urgent: false,
                covers: 1,
            });
        }
        count => raised.push(Coalesced {
            text: format!("{count} new remote requests"),
            urgent: false,
            covers: count,
        }),
    }

    raised
}

/// One notice, after coalescing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Coalesced {
    /// The line the human sees. Fixed local wording, names and counts only.
    pub text: String,
    /// Whether it needs them now.
    pub urgent: bool,
    /// How many messages it stands for.
    pub covers: usize,
}

#[cfg(test)]
mod tests;
