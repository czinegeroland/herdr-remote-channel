//! The approval state machine.
//!
//! Keyboard only, because PRD section 27 requires keyboard navigation for
//! interactive approval, and because a decision that can be reached by a
//! stray click is a decision that can be made by accident.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use hrc_core::gate::AgentView;
use hrc_herdr::LocalAgent;

/// One entry awaiting a human decision.
///
/// The metadata is the agent-safe view — the same closed set an agent would
/// see — and the body is held separately, because the whole point of this
/// screen is that the two are not the same thing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingItem {
    /// The message this is about.
    pub message_id: String,
    /// The metadata that is safe before approval.
    pub view: AgentView,
    /// The quarantined body. Displayed only after a deliberate reveal.
    pub body: String,
    /// What this machine found when it checked the body's claims.
    ///
    /// Local findings, never remote content, and drawn in a frame of their
    /// own so the two cannot be mistaken for each other. Empty for a message
    /// that makes no checkable claim.
    pub local_checks: Vec<String>,
}

/// Which part of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// Moving through the list of pending messages.
    List,
    /// Reading a revealed body.
    Body,
    /// Answering "are you sure".
    Confirm,
}

/// What the human decided, for the caller to carry out.
///
/// The interface does not act. It reports, and the daemon's trusted path
/// applies the decision — so the screen cannot become a second place where
/// approval rules live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Deliver the content as it arrived.
    DeliverToAgent {
        /// The message.
        message_id: String,
        /// The agent the human picked, as they saw it named.
        ///
        /// This is what the decision record carries, and a decision record
        /// can be read by an auditor — so it is the human-chosen label and
        /// never the local pane (PRD section 23.4).
        agent: String,
        /// What a local delivery is addressed to.
        ///
        /// Local topology: an agent name or a pane. It never leaves this
        /// machine and is not what the audit log records.
        target: String,
    },
    /// Keep it in the human inbox.
    KeepInInbox {
        /// The message.
        message_id: String,
    },
    /// Decline it.
    Decline {
        /// The message.
        message_id: String,
    },
    /// Leave the screen without deciding anything.
    Quit,
}

/// What the keys do once a body is on screen.
const KEYS_REVEALED: &str =
    "j/k/PgUp/PgDn: scroll   Tab: where to   a: deliver   i: inbox   d: decline   Esc: hide";

/// A decision awaiting confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Proposed {
    /// What would happen.
    pub outcome: Outcome,
    /// How it reads to the human, in words rather than in colour.
    pub prompt: String,
}

/// The trusted inbox screen.
#[derive(Debug, Clone)]
pub struct App {
    items: Vec<PendingItem>,
    selected: usize,
    revealed: bool,
    focus: Focus,
    proposed: Option<Proposed>,
    /// Every local place a delivery could go, enumerated by Herdr.
    ///
    /// Not a single fixed string any more. A person working in two
    /// workspaces has two answers to "where should this go", and picking for
    /// them was the screen deciding something only they can.
    destinations: Vec<LocalAgent>,
    /// Which of them is selected.
    destination: usize,
    body_scroll: Cell<u16>,
    body_page_rows: Cell<u16>,
    body_max_scroll: Cell<u16>,
    status: String,
}

impl App {
    /// Builds the screen over a set of pending items.
    ///
    /// Nothing is revealed and nothing is proposed. A screen that opened with
    /// a body on display, or with an approval preselected, would make the
    /// next key press consequential before the human had read anything.
    pub fn new(items: Vec<PendingItem>, destinations: Vec<LocalAgent>) -> Self {
        Self {
            items,
            selected: 0,
            revealed: false,
            focus: Focus::List,
            proposed: None,
            // Position zero is the session the person is working in, which
            // the discovery path sorts to the front. Preselecting it is not
            // deciding: nothing is proposed, nothing is revealed, and
            // delivering still takes three deliberate keys.
            destinations,
            destination: 0,
            body_scroll: Cell::new(0),
            body_page_rows: Cell::new(1),
            body_max_scroll: Cell::new(0),
            status: "Press Enter to reveal the selected message.".into(),
        }
    }

    /// The items on screen.
    pub fn items(&self) -> &[PendingItem] {
        &self.items
    }

    /// The selected item, if there is one.
    pub fn selected(&self) -> Option<&PendingItem> {
        self.items.get(self.selected)
    }

    /// The index of the selection.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Whether the selected body is being displayed.
    pub fn is_revealed(&self) -> bool {
        self.revealed
    }

    /// Where the keyboard is.
    pub fn focus(&self) -> Focus {
        self.focus
    }

    /// The decision awaiting confirmation, if any.
    pub fn proposed(&self) -> Option<&Proposed> {
        self.proposed.as_ref()
    }

    /// The line shown to the human explaining what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// The local destinations a delivery could go to.
    pub fn destinations(&self) -> &[LocalAgent] {
        &self.destinations
    }

    /// The destination a delivery would go to right now.
    pub fn destination(&self) -> Option<&LocalAgent> {
        self.destinations.get(self.destination)
    }

    /// Where the destination selection sits.
    pub fn destination_index(&self) -> usize {
        self.destination
    }

    /// How a destination reads to the person choosing it.
    ///
    /// The workspace is included when Herdr gave one, because two agents in
    /// two workspaces can carry the same name and the confirmation has to
    /// name the exact one.
    pub fn destination_label(agent: &LocalAgent) -> String {
        let named = match agent.local_workspace_id() {
            Some(workspace) => format!("{} in workspace {workspace}", agent.label()),
            None => agent.label().to_owned(),
        };

        // Said before the decision rather than discovered after it: a
        // person approving into a busy agent should know it will not land
        // straight away (docs/RESEARCH.md 5.4).
        if agent.is_working() {
            format!("{named} (working; waits until it is idle)")
        } else {
            named
        }
    }

    /// The first wrapped body row currently shown.
    pub fn body_scroll(&self) -> u16 {
        self.body_scroll.get()
    }

    /// Updates the rendered body dimensions and clamps scrolling after resize.
    pub(crate) fn set_body_viewport(&self, total_rows: usize, visible_rows: u16) {
        let max_scroll = total_rows
            .saturating_sub(visible_rows as usize)
            .min(u16::MAX as usize) as u16;
        self.body_page_rows.set(visible_rows.max(1));
        self.body_max_scroll.set(max_scroll);
        self.body_scroll.set(self.body_scroll.get().min(max_scroll));
    }

    /// Handles one key press, returning an outcome when one is reached.
    ///
    /// A decision needs two presses: the first proposes it and the second
    /// confirms it. Any other key cancels. That is what "manual approval"
    /// means in practice — not that a human was present, but that a human
    /// answered a question naming what was about to happen.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        // Windows reports a press and a release for the same physical key,
        // while Unix terminals report only the press. Counting both would
        // halve every interaction here: the press proposes a decision and the
        // release confirms it, so one keystroke would approve a disclosure.
        //
        // The guard lives in the state machine rather than in the runner
        // because it is the two-press rule that makes this screen trusted,
        // and a rule that depends on the caller filtering its input is a rule
        // the next caller breaks.
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(Outcome::Quit);
        }

        if self.focus == Focus::Confirm {
            return self.confirm(key);
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => {
                if self.revealed {
                    // Escape closes the body first, so leaving the screen
                    // takes a second, deliberate press.
                    self.revealed = false;
                    self.focus = Focus::List;
                    self.body_scroll.set(0);
                    self.status = "Press Enter to reveal the selected message.".into();
                    None
                } else {
                    Some(Outcome::Quit)
                }
            }

            KeyCode::Char('j') if self.focus == Focus::Body => {
                self.scroll_body(1);
                None
            }
            KeyCode::Char('k') if self.focus == Focus::Body => {
                self.scroll_body(-1);
                None
            }
            KeyCode::PageDown if self.focus == Focus::Body => {
                self.scroll_body(i32::from(self.body_page_rows.get()));
                None
            }
            KeyCode::PageUp if self.focus == Focus::Body => {
                self.scroll_body(-i32::from(self.body_page_rows.get()));
                None
            }
            KeyCode::Home if self.focus == Focus::Body => {
                self.body_scroll.set(0);
                None
            }
            KeyCode::End if self.focus == Focus::Body => {
                self.body_scroll.set(self.body_max_scroll.get());
                None
            }

            KeyCode::Down | KeyCode::Char('j') => {
                self.move_selection(1);
                None
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.move_selection(-1);
                None
            }

            KeyCode::Enter => {
                if self.selected().is_some() {
                    self.revealed = true;
                    self.focus = Focus::Body;
                    self.body_scroll.set(0);
                    self.status = KEYS_REVEALED.into();
                }
                None
            }

            // Choosing where it goes. Only once the body is on screen,
            // because a destination is half of a delivery decision and the
            // other half is having read what is being delivered.
            KeyCode::Tab if self.revealed => {
                self.cycle_destination(1);
                None
            }
            KeyCode::BackTab if self.revealed => {
                self.cycle_destination(-1);
                None
            }

            // Decisions are only reachable once the body is on screen. A
            // human approving something they have not been shown is the
            // failure this whole screen exists to prevent.
            KeyCode::Char('a') if self.revealed => {
                self.propose_delivery();
                None
            }
            KeyCode::Char('i') if self.revealed => {
                self.propose(|message_id| Outcome::KeepInInbox { message_id });
                None
            }
            KeyCode::Char('d') if self.revealed => {
                self.propose(|message_id| Outcome::Decline { message_id });
                None
            }

            _ => None,
        }
    }

    /// Moves the selection, closing any revealed body.
    fn move_selection(&mut self, delta: isize) {
        if self.items.is_empty() {
            return;
        }

        let last = self.items.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(1),
            _ => (self.selected + 1).min(last),
        };

        // Moving away hides the body. Otherwise a revealed pane and a moved
        // selection could disagree about which message is on screen, and a
        // decision would be made about the wrong one.
        self.revealed = false;
        self.focus = Focus::List;
        self.body_scroll.set(0);
        self.status = "Press Enter to reveal the selected message.".into();
    }

    /// Scrolls the wrapped body without changing which message is selected.
    fn scroll_body(&self, delta: i32) {
        let current = i32::from(self.body_scroll.get());
        let maximum = i32::from(self.body_max_scroll.get());
        self.body_scroll
            .set((current + delta).clamp(0, maximum) as u16);
    }

    /// Moves to the next or previous destination.
    ///
    /// Any pending confirmation is withdrawn. A confirmation names one exact
    /// destination, so a changed destination makes it a question about
    /// something else — and answering `y` to the old question would deliver
    /// somewhere the person did not read.
    fn cycle_destination(&mut self, delta: isize) {
        if self.destinations.len() < 2 {
            if self.destinations.is_empty() {
                self.status = "Herdr reported no local session to deliver to.".into();
            }
            return;
        }

        let length = self.destinations.len();
        self.destination = if delta < 0 {
            (self.destination + length - 1) % length
        } else {
            (self.destination + 1) % length
        };

        self.proposed = None;
        self.focus = Focus::Body;

        let Some(destination) = self.destination() else {
            return;
        };

        self.status = if destination.is_ready() {
            format!(
                "Delivery would go to {}.",
                Self::destination_label(destination)
            )
        } else {
            format!(
                "{} cannot take input right now.",
                Self::destination_label(destination)
            )
        };
    }

    /// Proposes delivering the revealed message to the chosen destination.
    ///
    /// Refuses rather than proposing when there is nowhere to deliver, or
    /// when Herdr says the chosen destination cannot take input. Proposing
    /// anyway would put a confirmation in front of a person that could only
    /// fail after they answered it, and the message would be left in a state
    /// neither of them chose.
    fn propose_delivery(&mut self) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };
        let sender = item.view.sender_local_name.clone();
        let message_id = item.message_id.clone();

        let Some(destination) = self.destinations.get(self.destination) else {
            self.status =
                "There is no local session to deliver to. Keep it in your inbox or decline it."
                    .into();
            return;
        };

        if !destination.is_ready() {
            self.status = format!(
                "{} cannot take input right now. Tab for another, or keep it in your inbox.",
                Self::destination_label(destination)
            );
            return;
        }

        let label = destination.label().to_owned();
        let prompt = format!(
            "Deliver this message from {sender} to {}? [y/N]",
            Self::destination_label(destination)
        );

        self.status = prompt.clone();
        self.proposed = Some(Proposed {
            outcome: Outcome::DeliverToAgent {
                message_id,
                agent: label,
                target: destination.local_target().to_owned(),
            },
            prompt,
        });
        self.focus = Focus::Confirm;
    }

    /// Proposes a decision that needs no destination.
    fn propose(&mut self, build: impl Fn(String) -> Outcome) {
        let Some(item) = self.items.get(self.selected) else {
            return;
        };

        let outcome = build(item.message_id.clone());
        let prompt = match &outcome {
            Outcome::DeliverToAgent { agent, .. } => format!(
                "Deliver this message from {} to {agent}? [y/N]",
                item.view.sender_local_name
            ),
            Outcome::KeepInInbox { .. } => {
                "Keep this message in your inbox, with no agent seeing it? [y/N]".into()
            }
            Outcome::Decline { .. } => "Decline this message? [y/N]".into(),
            Outcome::Quit => "Leave without deciding? [y/N]".into(),
        };

        self.status = prompt.clone();
        self.proposed = Some(Proposed { outcome, prompt });
        self.focus = Focus::Confirm;
    }

    /// Answers the confirmation.
    ///
    /// Only `y` confirms. Anything else cancels, including Enter — a key
    /// people press to dismiss things, which must not double as consent.
    fn confirm(&mut self, key: KeyEvent) -> Option<Outcome> {
        let proposed = self.proposed.take();
        self.focus = if self.revealed {
            Focus::Body
        } else {
            Focus::List
        };

        match key.code {
            KeyCode::Char('y') | KeyCode::Char('Y') => {
                self.status = "Decision recorded.".into();
                proposed.map(|proposed| proposed.outcome)
            }
            _ => {
                self.status = "Cancelled. Nothing was decided.".into();
                None
            }
        }
    }
}

impl crate::run::Screen for App {
    type Outcome = Outcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::render(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<Outcome> {
        App::on_key(self, key)
    }
}
