//! The channel setup screen.
//!
//! Creating a channel, issuing an invite and redeeming one are the three
//! steps that stand between installing this plugin and being able to say
//! anything. They were the last things that still required a shell.
//!
//! None of them is a trusted-human operation in the section 22.7 sense —
//! `hrc create`, `hrc invite create` and `hrc join` all run from an ordinary
//! terminal. What this screen adds is that they can be done from inside
//! Herdr, and that the two which cannot be undone say so before they run.
//!
//! An invite code is the one piece of output here that is a secret. It is
//! shown once so it can be passed to the person it is for, and the screen
//! says what it is rather than leaving a long base64 string to be guessed at.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// A step this screen can carry out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupStep {
    /// Create a channel backed by a Git repository.
    CreateChannel,
    /// Issue a single-use invite.
    CreateInvite,
    /// Redeem an invite issued by someone else.
    Join,
}

impl SetupStep {
    /// What a person sees.
    pub const fn title(self) -> &'static str {
        match self {
            SetupStep::CreateChannel => "Create a channel",
            SetupStep::CreateInvite => "Invite someone",
            SetupStep::Join => "Join with an invite code",
        }
    }

    /// What the step needs typed in.
    const fn prompt(self) -> &'static str {
        match self {
            SetupStep::CreateChannel => {
                "Git repository backing the channel, as owner/name or a path:"
            }
            SetupStep::CreateInvite => "GitHub user this invite is for:",
            SetupStep::Join => "Invite code:",
        }
    }

    /// Whether the typed value is a secret that should not be echoed.
    const fn is_secret(self) -> bool {
        // An invite code is a bearer secret: anyone holding it can present a
        // join request. It is typed where other people can see the screen.
        matches!(self, SetupStep::Join)
    }
}

/// What the human asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SetupOutcome {
    /// Create a channel on this repository.
    CreateChannel {
        /// Repository locator as typed.
        repo: String,
    },
    /// Issue an invite for this GitHub user.
    CreateInvite {
        /// The user the invite names.
        github_user: String,
    },
    /// Redeem this invite code.
    Join {
        /// The code as typed.
        invite_code: String,
    },
    /// Leave without doing anything.
    Quit,
}

/// Where the keyboard is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetupFocus {
    /// Choosing a step.
    Menu,
    /// Typing the value it needs.
    Input,
}

/// The setup screen.
#[derive(Debug, Clone)]
pub struct SetupApp {
    steps: Vec<SetupStep>,
    selected: usize,
    focus: SetupFocus,
    input: String,
    proposed: Option<String>,
    status: String,
}

impl SetupApp {
    /// Builds the screen over the steps that make sense right now.
    ///
    /// The caller decides which those are, because it is the only side that
    /// knows whether a channel already exists. Offering "create a channel"
    /// to an installation that has one, or "invite someone" to one that has
    /// no channel, would be offering a step that can only fail.
    pub fn new(steps: Vec<SetupStep>) -> Self {
        Self {
            steps,
            selected: 0,
            focus: SetupFocus::Menu,
            input: String::new(),
            proposed: None,
            status: "Enter: choose   Esc: leave".into(),
        }
    }

    /// The steps on offer.
    pub fn steps(&self) -> &[SetupStep] {
        &self.steps
    }

    /// The selected step, if there is one.
    pub fn selected(&self) -> Option<SetupStep> {
        self.steps.get(self.selected).copied()
    }

    /// The index of the selection.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// Where the keyboard is.
    pub fn focus(&self) -> SetupFocus {
        self.focus
    }

    /// The text typed so far, as it should be displayed.
    ///
    /// A secret is rendered as its length in asterisks rather than returned,
    /// so a screen cannot echo one by accident.
    pub fn displayed_input(&self) -> String {
        match self.selected().is_some_and(SetupStep::is_secret) {
            true => "*".repeat(self.input.chars().count()),
            false => self.input.clone(),
        }
    }

    /// The prompt for the selected step.
    pub fn prompt(&self) -> &'static str {
        self.selected().map_or("", SetupStep::prompt)
    }

    /// The step awaiting confirmation, if any.
    pub fn proposed(&self) -> Option<&str> {
        self.proposed.as_deref()
    }

    /// The line telling the human what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<SetupOutcome> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(SetupOutcome::Quit);
        }

        if self.proposed.is_some() {
            return self.confirm(key);
        }

        match self.focus {
            SetupFocus::Menu => self.on_menu_key(key),
            SetupFocus::Input => self.on_input_key(key),
        }
    }

    /// Keys while choosing a step.
    fn on_menu_key(&mut self, key: KeyEvent) -> Option<SetupOutcome> {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => Some(SetupOutcome::Quit),
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
                    self.focus = SetupFocus::Input;
                    self.input.clear();
                    self.status = "Type the value, then Enter. Esc: back".into();
                }
                None
            }
            _ => None,
        }
    }

    /// Keys while typing a value.
    fn on_input_key(&mut self, key: KeyEvent) -> Option<SetupOutcome> {
        match key.code {
            KeyCode::Esc => {
                self.focus = SetupFocus::Menu;
                self.input.clear();
                self.status = "Enter: choose   Esc: leave".into();
                None
            }
            KeyCode::Backspace => {
                self.input.pop();
                None
            }
            KeyCode::Enter => {
                self.propose();
                None
            }
            KeyCode::Char(character) => {
                self.input.push(character);
                None
            }
            _ => None,
        }
    }

    /// Answers the confirmation. Only `y` proceeds.
    fn confirm(&mut self, key: KeyEvent) -> Option<SetupOutcome> {
        let _ = self.proposed.take();

        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.status = "Cancelled. Nothing was done.".into();
            return None;
        }

        let step = self.selected()?;
        let value = self.input.clone();

        Some(match step {
            SetupStep::CreateChannel => SetupOutcome::CreateChannel { repo: value },
            SetupStep::CreateInvite => SetupOutcome::CreateInvite { github_user: value },
            SetupStep::Join => SetupOutcome::Join { invite_code: value },
        })
    }

    /// Moves the selection.
    fn move_selection(&mut self, delta: isize) {
        if self.steps.is_empty() {
            return;
        }

        let last = self.steps.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(1),
            _ => (self.selected + 1).min(last),
        };
    }

    /// Proposes the step, or explains why it cannot run.
    fn propose(&mut self) {
        let Some(step) = self.selected() else {
            return;
        };

        if self.input.trim().is_empty() {
            self.status = "Nothing typed yet.".into();
            return;
        }

        // Creating a channel publishes a genesis object to a repository, and
        // redeeming an invite spends it. Neither can be taken back, so both
        // say what they are about to touch rather than asking whether the
        // person is sure.
        self.proposed = Some(match step {
            SetupStep::CreateChannel => format!(
                "Create a channel on {}? This publishes to that repository. [y/N]",
                self.input
            ),
            SetupStep::CreateInvite => {
                format!("Issue a single-use invite for {}? [y/N]", self.input)
            }
            // The code is not echoed back here either.
            SetupStep::Join => {
                "Redeem this invite code? It can only be used once. [y/N]".to_owned()
            }
        });
    }
}

impl crate::run::Screen for SetupApp {
    type Outcome = SetupOutcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_setup(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<SetupOutcome> {
        SetupApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
