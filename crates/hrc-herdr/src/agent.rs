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
}

impl LocalAgent {
    /// Registers a locally discovered agent.
    pub fn new(pane_id: impl Into<String>, label: impl Into<String>) -> Self {
        Self {
            pane_id: pane_id.into(),
            label: label.into(),
        }
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
