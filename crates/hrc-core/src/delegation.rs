//! Tracking a delegated task without executing it.
//!
//! PRD section 5: delegation is communication. This module holds the state
//! of a task as both sides report it, and the rules about who may report
//! what. It starts nothing, runs nothing, and reads nothing from a
//! repository.
//!
//! Two rules do the work. A transition must be legal in the lifecycle of
//! PRD section 18.4, which [`DelegationState::may_precede`] decides. And it
//! must come from the party entitled to make it: an assignee accepts,
//! progresses, and reports a result, while a requester cancels and judges a
//! result complete. Without the second rule, a peer could accept a task on
//! someone else's behalf, or declare its own work finished and reviewed.

use hrc_protocol::delegation::DelegationState;

use crate::error::{CoreError, Result};

/// Which side of a delegation a principal is on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Party {
    /// The principal who asked.
    Requester,
    /// The principal asked to do it.
    Assignee,
}

/// One delegated task, as this installation understands it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Delegation {
    /// The `task` message that opened it.
    pub task_id: String,
    /// The principal who asked.
    pub requester: String,
    /// The principal asked to do it.
    pub assignee: String,
    /// Where it stands.
    pub state: DelegationState,
}

impl Delegation {
    /// Opens a delegation in the `requested` state.
    pub fn open(
        task_id: impl Into<String>,
        requester: impl Into<String>,
        assignee: impl Into<String>,
    ) -> Self {
        Self {
            task_id: task_id.into(),
            requester: requester.into(),
            assignee: assignee.into(),
            state: DelegationState::Requested,
        }
    }

    /// Which side a principal is on, if either.
    pub fn party_of(&self, principal_id: &str) -> Option<Party> {
        if principal_id == self.assignee {
            Some(Party::Assignee)
        } else if principal_id == self.requester {
            Some(Party::Requester)
        } else {
            None
        }
    }

    /// Applies a reported state change.
    ///
    /// Checked in this order, because each step decides what the next is
    /// allowed to assume: is the reporter part of this delegation at all, is
    /// the transition legal from where the task stands, and is this reporter
    /// the party entitled to make it.
    pub fn apply(&mut self, reporter: &str, next: DelegationState) -> Result<()> {
        let party = self
            .party_of(reporter)
            .ok_or_else(|| CoreError::NotADelegationParty {
                principal_id: reporter.to_owned(),
                task_id: self.task_id.clone(),
            })?;

        if !self.state.may_precede(next) {
            return Err(CoreError::IllegalDelegationTransition {
                task_id: self.task_id.clone(),
                from: self.state.as_str(),
                to: next.as_str(),
            });
        }

        if !may_report(party, next) {
            return Err(CoreError::WrongDelegationParty {
                task_id: self.task_id.clone(),
                principal_id: reporter.to_owned(),
                state: next.as_str(),
            });
        }

        self.state = next;
        Ok(())
    }

    /// Whether the task is over.
    pub fn is_finished(&self) -> bool {
        self.state.is_terminal()
    }
}

/// Whether a party may report a given state.
///
/// The division follows who the statement is actually about. Taking a task
/// on, working it, and reporting an outcome are the assignee's to say.
/// Withdrawing the request and accepting the outcome as done are the
/// requester's. Expiry belongs to neither — it is what happens when nobody
/// says anything — so it is not reportable by a peer at all.
fn may_report(party: Party, state: DelegationState) -> bool {
    use DelegationState::*;

    match state {
        Accepted | Declined | InProgress | NeedsInput | ResultPendingReview | Failed => {
            party == Party::Assignee
        }
        Cancelled | Completed => party == Party::Requester,
        Requested | Expired => false,
    }
}

#[cfg(test)]
mod tests;
