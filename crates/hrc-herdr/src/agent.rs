//! Choosing a local agent for approved prompt delivery (PRD section 23.4).
//!
//! The rule this module exists to hold is a negative one: *remote
//! participants never receive local pane IDs or complete local agent
//! listings* (PRD section 4 constraint 8, section 23.4). A sender may say
//! which endpoint they are addressing; they may not learn what is running
//! here, and they may certainly not name the pane their content lands in.
//!
//! So [`LocalAgent`] deliberately does not implement `Serialize`. The type
//! that holds a pane ID cannot be turned into JSON, which means it cannot be
//! put in a message body, an envelope field, a receipt, or a notification
//! without someone first writing the conversion by hand and explaining why.
//! [`LocalAgent::disclosable_label`] is that conversion, and it discloses
//! only the human-chosen label.

/// A local agent the human can deliver approved content to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalAgent {
    /// The local pane this agent occupies.
    ///
    /// Local-only. Never crosses the channel.
    pane_id: String,
    /// The name the local human knows it by.
    label: String,
    /// What `agent.prompt` is addressed to: the agent's unique live name
    /// when it has one, and the pane it occupies otherwise.
    ///
    /// Local-only, for the same reason `pane_id` is. It is separate from
    /// `pane_id` because Herdr resolves a named agent through its name even
    /// after it moves between panes, and addressing the pane instead would
    /// deliver to whatever occupies it now.
    target: String,
    /// The local workspace it belongs to, when Herdr said.
    ///
    /// Local-only. Shown so that two agents with the same name in different
    /// workspaces are distinguishable on the confirmation.
    workspace_id: Option<String>,
    /// Whether this is the session the person is working in.
    current: bool,
    /// Whether Herdr says it can take input right now.
    ready: bool,
    /// Whether Herdr says it is in the middle of a turn.
    working: bool,
}

impl LocalAgent {
    /// Registers a locally discovered agent.
    ///
    /// The pane doubles as the delivery target and the agent is assumed
    /// ready, which is what a caller that knows nothing more can say. The
    /// discovery path in [`crate::host`] fills the rest in from what Herdr
    /// actually reported.
    pub fn new(pane_id: impl Into<String>, label: impl Into<String>) -> Self {
        let pane_id = pane_id.into();
        Self {
            target: pane_id.clone(),
            pane_id,
            label: label.into(),
            workspace_id: None,
            current: false,
            ready: true,
            working: false,
        }
    }

    /// Sets what `agent.prompt` is addressed to.
    #[must_use]
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = target.into();
        self
    }

    /// Records the local workspace this agent belongs to.
    #[must_use]
    pub fn in_workspace(mut self, workspace_id: Option<String>) -> Self {
        self.workspace_id = workspace_id;
        self
    }

    /// Marks this as the session the person is working in.
    #[must_use]
    pub fn as_current(mut self, current: bool) -> Self {
        self.current = current;
        self
    }

    /// Records whether Herdr says it can take input right now.
    #[must_use]
    pub fn when_ready(mut self, ready: bool) -> Self {
        self.ready = ready;
        self
    }

    /// Records whether Herdr says it is in the middle of a turn.
    #[must_use]
    pub fn when_working(mut self, working: bool) -> Self {
        self.working = working;
        self
    }

    /// What a local delivery is addressed to.
    ///
    /// Named for what it must not be used for, like [`Self::local_pane_id`].
    pub fn local_target(&self) -> &str {
        &self.target
    }

    /// The local workspace, for telling two same-named agents apart.
    pub fn local_workspace_id(&self) -> Option<&str> {
        self.workspace_id.as_deref()
    }

    /// Whether this is the session the person is working in.
    pub fn is_current(&self) -> bool {
        self.current
    }

    /// Whether Herdr said it can take input right now.
    ///
    /// A blocked or not-yet-interactive agent is still listed rather than
    /// hidden, because a destination that silently disappears is harder to
    /// understand than one that says why it cannot be chosen.
    pub fn is_ready(&self) -> bool {
        self.ready
    }

    /// Whether Herdr said it is in the middle of a turn.
    ///
    /// Such an agent can take input -- a prompt typed into a working agent is
    /// queued by most of them -- but content landing mid-turn arrives too
    /// late to inform what the turn is doing and interrupts whoever is
    /// watching it. The review holds an approved delivery until it is idle
    /// (docs/RESEARCH.md 5.4).
    pub fn is_working(&self) -> bool {
        self.working
    }

    /// The pane this agent occupies, for local delivery only.
    ///
    /// Named for what it must not be used for. Every caller is local: the
    /// daemon writing approved content into a pane, and the plugin drawing
    /// the selection list.
    pub fn local_pane_id(&self) -> &str {
        &self.pane_id
    }

    /// The label shown to the local human.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The only part of an agent that may leave this installation.
    ///
    /// A decision record names the agent a human chose, and that record can
    /// be read by an auditor. The label is human-chosen; the pane ID is
    /// local topology.
    pub fn disclosable_label(&self) -> &str {
        &self.label
    }
}

/// The agents available locally, and which one is selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Selection {
    agents: Vec<LocalAgent>,
    selected: Option<usize>,
}

impl Selection {
    /// Builds a selection over the locally discovered agents.
    ///
    /// Nothing is selected initially. Section 19.2 requires the trusted
    /// screen to open with no decision selected, and delivering to whichever
    /// agent happened to sort first would be a decision.
    pub fn new(agents: Vec<LocalAgent>) -> Self {
        Self {
            agents,
            selected: None,
        }
    }

    /// The agents, for the local selection list.
    pub fn agents(&self) -> &[LocalAgent] {
        &self.agents
    }

    /// Selects the agent at this position, if there is one.
    pub fn select(&mut self, index: usize) -> bool {
        if index < self.agents.len() {
            self.selected = Some(index);
            return true;
        }

        false
    }

    /// Clears the selection.
    pub fn clear(&mut self) {
        self.selected = None;
    }

    /// The selected agent, if the human has chosen one.
    pub fn selected(&self) -> Option<&LocalAgent> {
        self.selected.and_then(|index| self.agents.get(index))
    }

    /// Whether an approved prompt can be delivered right now.
    ///
    /// A prompt request cannot be approved into nowhere: with no agent
    /// chosen there is no destination, and the delivery decision is not
    /// available.
    pub fn ready_for_delivery(&self) -> bool {
        self.selected().is_some()
    }
}

#[cfg(test)]
mod tests;
