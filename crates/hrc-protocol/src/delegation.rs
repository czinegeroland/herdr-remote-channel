//! Delegation bodies and the lifecycle they report.
//!
//! PRD section 5 is explicit that delegation is in scope "only as
//! communication": a request, an acceptance, progress, and a result. It does
//! not start agents and does not modify repositories.
//!
//! That constraint shapes these types. A task carries a title, a description,
//! acceptance criteria, and a reference to context the sender already chose
//! to share. There is no command, no script, no path, and no argument list —
//! not because a receiver would run one, but because a field that looks
//! runnable is an invitation for some future caller to run it. The absence is
//! the safety property, so a test asserts it.

use serde::{Deserialize, Serialize};

use crate::error::{ProtocolError, Result};

/// Where a delegated task stands (PRD section 18.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationState {
    /// The requester has asked.
    Requested,
    /// The assignee took it on.
    Accepted,
    /// The assignee refused it.
    Declined,
    /// Work is under way.
    InProgress,
    /// The assignee is blocked on the requester.
    NeedsInput,
    /// A result exists and a human has not reviewed it yet.
    ResultPendingReview,
    /// Reviewed and finished.
    Completed,
    /// Attempted and unsuccessful.
    Failed,
    /// Withdrawn.
    Cancelled,
    /// Left too long.
    Expired,
}

impl DelegationState {
    /// Every state, in the order PRD section 18.4 lists them.
    pub const ALL: [DelegationState; 10] = [
        DelegationState::Requested,
        DelegationState::Accepted,
        DelegationState::Declined,
        DelegationState::InProgress,
        DelegationState::NeedsInput,
        DelegationState::ResultPendingReview,
        DelegationState::Completed,
        DelegationState::Failed,
        DelegationState::Cancelled,
        DelegationState::Expired,
    ];

    /// The wire representation.
    pub const fn as_str(self) -> &'static str {
        match self {
            DelegationState::Requested => "requested",
            DelegationState::Accepted => "accepted",
            DelegationState::Declined => "declined",
            DelegationState::InProgress => "in_progress",
            DelegationState::NeedsInput => "needs_input",
            DelegationState::ResultPendingReview => "result_pending_review",
            DelegationState::Completed => "completed",
            DelegationState::Failed => "failed",
            DelegationState::Cancelled => "cancelled",
            DelegationState::Expired => "expired",
        }
    }

    /// Parses a wire representation, returning `None` for anything else.
    pub fn parse(value: &str) -> Option<DelegationState> {
        DelegationState::ALL
            .into_iter()
            .find(|state| state.as_str() == value)
    }

    /// Whether the task is over.
    ///
    /// A terminal state accepts no further transitions. Without that, a peer
    /// could reopen a task someone had already completed or withdrawn, and a
    /// requester's record of what happened would depend on who spoke last.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            DelegationState::Declined
                | DelegationState::Completed
                | DelegationState::Failed
                | DelegationState::Cancelled
                | DelegationState::Expired
        )
    }

    /// Whether this state may follow `self`.
    pub fn may_precede(self, next: DelegationState) -> bool {
        use DelegationState::*;

        if self.is_terminal() {
            return false;
        }

        match self {
            Requested => matches!(next, Accepted | Declined | Cancelled | Expired),
            Accepted => matches!(
                next,
                InProgress | NeedsInput | ResultPendingReview | Failed | Cancelled | Expired
            ),
            InProgress | NeedsInput => matches!(
                next,
                InProgress | NeedsInput | ResultPendingReview | Failed | Cancelled | Expired
            ),
            ResultPendingReview => matches!(next, Completed | Failed | Cancelled | Expired),
            Declined | Completed | Failed | Cancelled | Expired => false,
        }
    }
}

/// How long a delegation field may be.
///
/// Generous for prose, bounded so one message cannot make a reviewing human
/// scroll through a megabyte of sender-chosen text.
pub const MAX_TEXT_BYTES: usize = 16 * 1024;

/// The body of a `task` message.
///
/// Note what is not here: no command, no working directory, no arguments.
/// Delegation is a request to a person, and the shape says so.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskBody {
    /// A short summary for a list.
    pub title: String,
    /// What is being asked for.
    pub description: String,
    /// How the requester will judge the result.
    #[serde(rename = "acceptanceCriteria", default)]
    pub acceptance_criteria: Vec<String>,
    /// A context package already shared, by its identifier.
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// When the requester stops waiting.
    #[serde(rename = "dueBy", skip_serializing_if = "Option::is_none")]
    pub due_by: Option<String>,
}

/// The body of a `progress` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProgressBody {
    /// The task being reported on.
    #[serde(rename = "taskId")]
    pub task_id: String,
    /// The state now reached.
    pub state: DelegationState,
    /// What the assignee wants the requester to know.
    #[serde(default)]
    pub note: String,
}

/// The body of a `result` message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultBody {
    /// The task this concludes.
    #[serde(rename = "taskId")]
    pub task_id: String,
    /// Whether the assignee considers it done or failed.
    pub state: DelegationState,
    /// The assignee's account of the outcome.
    pub summary: String,
    /// A context package carrying the output, by its identifier.
    #[serde(rename = "contextId", skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    /// Things the result claims exist, stated so a receiver can check them.
    ///
    /// A reference is a claim, not an instruction. Nothing on the receiving
    /// side fetches, checks out or applies it; the trusted review only says
    /// whether it already resolves in the receiver's own checkout.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub references: Vec<Reference>,
}

/// The most references one result may carry.
pub const MAX_REFERENCES: usize = 16;

/// Something a result claims exists, in a form the receiver can check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reference {
    /// What kind of thing `id` names. `commit` is the only kind defined.
    ///
    /// A string rather than a closed enum, so that a receiver meeting a kind
    /// added later still reads the rest of the result and can say it did not
    /// check that one, instead of refusing the whole message.
    pub kind: String,
    /// The identifier, in the kind's canonical form.
    pub id: String,
}

/// The one reference kind this version defines: a Git commit.
pub const REFERENCE_COMMIT: &str = "commit";

impl Reference {
    /// A Git commit, by its full object name.
    pub fn commit(id: impl Into<String>) -> Self {
        Self {
            kind: REFERENCE_COMMIT.to_owned(),
            id: id.into(),
        }
    }

    /// Checks the reference's shape.
    ///
    /// A commit must be a full, lowercase object name — forty hex digits for
    /// SHA-1 or sixty-four for SHA-256. An abbreviation is refused because it
    /// can resolve to a different object in a different repository, and a
    /// check that passed by matching the wrong commit would be worse than no
    /// check. A kind this version does not define is only length-bounded.
    pub fn validate(&self) -> Result<()> {
        bounded("references.kind", &self.kind, true)?;
        bounded("references.id", &self.id, true)?;

        if self.kind == REFERENCE_COMMIT {
            let expected_bytes = if self.id.len() == 64 { 32 } else { 20 };
            let hex = self
                .id
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));

            if !hex || self.id.len() != expected_bytes * 2 {
                return Err(ProtocolError::Hex {
                    field: "references.id",
                    expected_bytes,
                });
            }
        }

        Ok(())
    }
}

/// Checks one bounded text field.
fn bounded(field: &'static str, value: &str, required: bool) -> Result<()> {
    if required && value.trim().is_empty() {
        return Err(ProtocolError::MissingField { field });
    }

    if value.len() > MAX_TEXT_BYTES {
        return Err(ProtocolError::MessageTooLarge {
            size: value.len(),
            limit: MAX_TEXT_BYTES,
        });
    }

    Ok(())
}

impl TaskBody {
    /// Checks the invariants a receiver must not assume.
    pub fn validate(&self) -> Result<()> {
        bounded("title", &self.title, true)?;
        bounded("description", &self.description, true)?;

        for criterion in &self.acceptance_criteria {
            bounded("acceptanceCriteria", criterion, true)?;
        }

        Ok(())
    }
}

impl ProgressBody {
    /// Checks the invariants a receiver must not assume.
    pub fn validate(&self) -> Result<()> {
        bounded("taskId", &self.task_id, true)?;
        bounded("note", &self.note, false)?;

        // A progress message reporting a conclusion would bypass the result
        // message a human is supposed to review.
        if self.state.is_terminal() && self.state != DelegationState::Cancelled {
            return Err(ProtocolError::DerivedMismatch { field: "state" });
        }

        Ok(())
    }
}

impl ResultBody {
    /// Checks the invariants a receiver must not assume.
    pub fn validate(&self) -> Result<()> {
        bounded("taskId", &self.task_id, true)?;
        bounded("summary", &self.summary, true)?;

        if !matches!(
            self.state,
            DelegationState::ResultPendingReview | DelegationState::Failed
        ) {
            // An assignee reports a result or a failure. Declaring it
            // *completed* is the requester's judgement, not theirs.
            return Err(ProtocolError::DerivedMismatch { field: "state" });
        }

        if self.references.len() > MAX_REFERENCES {
            return Err(ProtocolError::MessageTooLarge {
                size: self.references.len(),
                limit: MAX_REFERENCES,
            });
        }

        for reference in &self.references {
            reference.validate()?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::canonical;

    fn task() -> TaskBody {
        TaskBody {
            title: "Review the retry backoff".into(),
            description: "The staging deploy retries four times in a second.".into(),
            acceptance_criteria: vec!["A test covers the backoff interval".into()],
            context_id: Some("ctx-1".into()),
            due_by: None,
        }
    }

    #[test]
    fn a_task_round_trips_in_the_documented_shape() {
        let encoded = canonical::to_canonical_json(&task()).unwrap();
        let value: serde_json::Value = serde_json::from_str(&encoded).unwrap();

        assert_eq!(value["title"], "Review the retry backoff");
        assert!(value["acceptanceCriteria"].is_array());
        assert!(
            value.get("dueBy").is_none(),
            "an absent field must not be null"
        );

        let decoded: TaskBody = canonical::from_json_str(&encoded).unwrap();
        assert_eq!(decoded, task());
    }

    #[test]
    fn a_task_carries_nothing_that_looks_executable() {
        // The safety property of PRD section 5, enforced on the shape rather
        // than on the behavior of whoever reads it. A field named like a
        // command is an invitation for some future caller to run one.
        let encoded = canonical::to_canonical_json(&task()).unwrap();

        for forbidden in [
            "command",
            "cmd",
            "script",
            "exec",
            "run",
            "shell",
            "argv",
            "args",
            "entrypoint",
            "interpreter",
            "workingDirectory",
            "env",
        ] {
            assert!(
                !encoded.contains(&format!("\"{forbidden}\"")),
                "a task body exposes a `{forbidden}` field"
            );
        }
    }

    #[test]
    fn every_documented_state_parses() {
        for state in DelegationState::ALL {
            assert_eq!(DelegationState::parse(state.as_str()), Some(state));
        }
        assert_eq!(DelegationState::parse("running"), None);
    }

    #[test]
    fn the_lifecycle_follows_section_18_4() {
        use DelegationState::*;

        assert!(Requested.may_precede(Accepted));
        assert!(Requested.may_precede(Declined));
        assert!(Accepted.may_precede(InProgress));
        assert!(InProgress.may_precede(NeedsInput));
        assert!(NeedsInput.may_precede(InProgress));
        assert!(InProgress.may_precede(ResultPendingReview));
        assert!(ResultPendingReview.may_precede(Completed));
        assert!(ResultPendingReview.may_precede(Failed));

        // Skipping the request, and skipping review.
        assert!(!Requested.may_precede(InProgress));
        assert!(!Requested.may_precede(Completed));
        assert!(!Accepted.may_precede(Completed));
        assert!(!InProgress.may_precede(Completed));
    }

    #[test]
    fn a_finished_task_cannot_be_reopened() {
        // Otherwise a peer could revive a task the requester had closed, and
        // the record of what happened would depend on who spoke last.
        for terminal in [
            DelegationState::Declined,
            DelegationState::Completed,
            DelegationState::Failed,
            DelegationState::Cancelled,
            DelegationState::Expired,
        ] {
            assert!(terminal.is_terminal());

            for next in DelegationState::ALL {
                assert!(
                    !terminal.may_precede(next),
                    "{terminal:?} should not be followed by {next:?}"
                );
            }
        }
    }

    #[test]
    fn cancellation_and_expiry_are_reachable_from_every_live_state() {
        for state in DelegationState::ALL {
            if state.is_terminal() {
                continue;
            }
            assert!(state.may_precede(DelegationState::Cancelled), "{state:?}");
            assert!(state.may_precede(DelegationState::Expired), "{state:?}");
        }
    }

    #[test]
    fn a_task_needs_a_title_and_a_description() {
        let mut empty = task();
        empty.title = "   ".into();
        assert!(empty.validate().is_err());

        let mut blank = task();
        blank.description = String::new();
        assert!(blank.validate().is_err());

        task().validate().unwrap();
    }

    #[test]
    fn oversized_prose_is_refused() {
        let mut huge = task();
        huge.description = "x".repeat(MAX_TEXT_BYTES + 1);

        assert!(matches!(
            huge.validate().unwrap_err(),
            ProtocolError::MessageTooLarge { .. }
        ));
    }

    #[test]
    fn progress_cannot_announce_a_conclusion() {
        // A conclusion arrives as a result, which is what a human reviews.
        for state in [DelegationState::Completed, DelegationState::Failed] {
            let progress = ProgressBody {
                task_id: "01ARZ3".into(),
                state,
                note: String::new(),
            };
            assert!(progress.validate().is_err(), "{state:?}");
        }

        ProgressBody {
            task_id: "01ARZ3".into(),
            state: DelegationState::InProgress,
            note: "halfway".into(),
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn an_assignee_cannot_declare_its_own_work_complete() {
        // `completed` is the requester's judgement after review.
        let overreach = ResultBody {
            task_id: "01ARZ3".into(),
            state: DelegationState::Completed,
            summary: "done".into(),
            context_id: None,
            references: Vec::new(),
        };
        assert!(overreach.validate().is_err());

        ResultBody {
            task_id: "01ARZ3".into(),
            state: DelegationState::ResultPendingReview,
            summary: "here is what I found".into(),
            context_id: Some("ctx-2".into()),
            references: vec![Reference::commit("3f2a91c0".repeat(5))],
        }
        .validate()
        .unwrap();
    }

    fn result_with(references: Vec<Reference>) -> ResultBody {
        ResultBody {
            task_id: "01ARZ3".into(),
            state: DelegationState::ResultPendingReview,
            summary: "the fix is on main".into(),
            context_id: None,
            references,
        }
    }

    #[test]
    fn a_result_without_references_keeps_its_original_shape() {
        // Existing senders and receivers must see the same bytes: the field
        // is omitted, not written as an empty list.
        let encoded = canonical::to_canonical_json(&result_with(Vec::new())).unwrap();
        assert!(!encoded.contains("references"), "{encoded}");

        let decoded: ResultBody =
            serde_json::from_str(r#"{"state":"result_pending_review","summary":"s","taskId":"t"}"#)
                .unwrap();
        assert!(decoded.references.is_empty());
    }

    #[test]
    fn a_commit_reference_must_be_a_full_lowercase_object_name() {
        let sha1 = "a".repeat(40);
        let sha256 = "b".repeat(64);
        result_with(vec![Reference::commit(&sha1), Reference::commit(&sha256)])
            .validate()
            .unwrap();

        // An abbreviation could match a different object elsewhere.
        for bad in [
            "3f2a91c",
            &"A".repeat(40),
            &"g".repeat(40),
            &"a".repeat(41),
            "",
        ] {
            assert!(
                result_with(vec![Reference::commit(bad)])
                    .validate()
                    .is_err(),
                "{bad:?} was accepted"
            );
        }
    }

    #[test]
    fn a_reference_of_an_unknown_kind_is_carried_rather_than_refused() {
        let later = Reference {
            kind: "ci_run".into(),
            id: "12345".into(),
        };
        result_with(vec![later]).validate().unwrap();
    }

    #[test]
    fn a_result_carries_a_bounded_number_of_references() {
        let many = vec![Reference::commit("c".repeat(40)); MAX_REFERENCES + 1];
        assert!(result_with(many).validate().is_err());
    }
}
