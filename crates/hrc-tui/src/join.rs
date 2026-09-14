//! The membership approval screen.
//!
//! PRD requirement HRC-CH-007: joining a channel needs explicit
//! administrator approval. The decision is the same shape as the one in
//! [`crate::app`] — reveal nothing by default, two presses to decide — but
//! what the human is checking is different, and that difference is the whole
//! point of a separate screen.
//!
//! Approving a message is a judgement about *content*. Admitting a member is
//! a judgement about *identity*, and the only thing that establishes it is
//! the safety phrase: a short wordlist rendering of the joiner's key material
//! that both sides can read aloud over a channel an attacker does not
//! control. An administrator who admits without comparing it has verified
//! nothing, whatever the screen said.
//!
//! So the phrase is displayed in full, and the confirmation names it rather
//! than asking "are you sure". A prompt that does not say what was supposed
//! to be checked trains people to say yes.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One joiner waiting to be admitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingJoin {
    /// The request this is about.
    pub request_id: String,
    /// The principal asking to join, as the proof establishes it.
    pub principal_id: String,
    /// The device the request came from.
    pub device_id: String,
    /// When the joiner says they asked.
    pub created_at: String,
    /// The phrase both sides compare out of band.
    pub safety_phrase: String,
}

/// What the administrator decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinOutcome {
    /// Admit the joiner to the channel.
    Admit {
        /// Which request.
        request_id: String,
    },
    /// Refuse the request.
    Refuse {
        /// Which request.
        request_id: String,
    },
    /// Leave without deciding.
    Quit,
}

/// A decision awaiting confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProposedJoin {
    /// What would happen.
    pub outcome: JoinOutcome,
    /// How it reads to the human.
    pub prompt: String,
}

/// The membership approval screen.
#[derive(Debug, Clone)]
pub struct JoinApp {
    requests: Vec<PendingJoin>,
    selected: usize,
    proposed: Option<ProposedJoin>,
    channel_local_name: String,
    status: String,
}

impl JoinApp {
    /// Builds the screen over the requests that already validate.
    ///
    /// Only requests whose proof checked out reach here; an unverifiable one
    /// is not something to put in front of a human, because there is nothing
    /// they could usefully decide about it.
    pub fn new(requests: Vec<PendingJoin>, channel_local_name: impl Into<String>) -> Self {
        Self {
            requests,
            selected: 0,
            proposed: None,
            channel_local_name: channel_local_name.into(),
            status: "Compare the safety phrase with the joiner before admitting. \
                     a: admit   r: refuse   q: leave"
                .into(),
        }
    }

    /// The requests on screen.
    pub fn requests(&self) -> &[PendingJoin] {
        &self.requests
    }

    /// The selected request, if there is one.
    pub fn selected(&self) -> Option<&PendingJoin> {
        self.requests.get(self.selected)
    }

    /// The index of the selection.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The decision awaiting confirmation, if any.
    pub fn proposed(&self) -> Option<&ProposedJoin> {
        self.proposed.as_ref()
    }

    /// The channel this screen is admitting to.
    pub fn channel_local_name(&self) -> &str {
        &self.channel_local_name
    }

    /// The line telling the human what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Handles one key press.
    ///
    /// Admitting takes two presses, like every other decision here. There is
    /// no reveal step because nothing is hidden: the safety phrase has to be
    /// on screen for the administrator to compare it, and a phrase behind a
    /// key press is a phrase people skip.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<JoinOutcome> {
        // Windows reports a press and a release for the same physical key.
        // Counting both would let one keystroke both propose and confirm an
        // admission, which is the same hazard the message screen guards.
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(JoinOutcome::Quit);
        }

        if self.proposed.is_some() {
            return self.confirm(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => Some(JoinOutcome::Quit),
            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                None
            }
            KeyCode::Char('a') => {
                self.propose(true);
                None
            }
            KeyCode::Char('r') => {
                self.propose(false);
                None
            }
            _ => None,
        }
    }

    /// Answers the confirmation.
    ///
    /// Only `y` confirms. Anything else cancels, so a mistyped key never
    /// admits anyone.
    fn confirm(&mut self, key: KeyEvent) -> Option<JoinOutcome> {
        let proposed = self.proposed.take()?;

        if matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            return Some(proposed.outcome);
        }

        self.status = "Cancelled. Nothing was decided.".into();
        None
    }

    /// Moves the selection, cancelling any proposal.
    fn move_selection(&mut self, delta: isize) {
        if self.requests.is_empty() {
            return;
        }

        let last = self.requests.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(1),
            _ => (self.selected + 1).min(last),
        };
        self.proposed = None;
    }

    /// Proposes admitting or refusing the selected request.
    fn propose(&mut self, admit: bool) {
        let Some(request) = self.requests.get(self.selected) else {
            return;
        };

        let outcome = if admit {
            JoinOutcome::Admit {
                request_id: request.request_id.clone(),
            }
        } else {
            JoinOutcome::Refuse {
                request_id: request.request_id.clone(),
            }
        };

        // The prompt names the safety phrase on admission, because that is
        // the thing the administrator was supposed to have checked. "Are you
        // sure?" would be a question about their confidence rather than about
        // the evidence.
        let prompt = if admit {
            format!(
                "Admit this device to {}? Confirm the joiner read back: {} [y/N]",
                self.channel_local_name, request.safety_phrase
            )
        } else {
            format!(
                "Refuse this join request to {}? [y/N]",
                self.channel_local_name
            )
        };

        self.proposed = Some(ProposedJoin { outcome, prompt });
    }
}

impl crate::run::Screen for JoinApp {
    type Outcome = JoinOutcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_joins(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<JoinOutcome> {
        JoinApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
