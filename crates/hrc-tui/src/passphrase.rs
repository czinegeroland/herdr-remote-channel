//! The passphrase prompt.
//!
//! Most things this plugin does need the key store open: signing a message,
//! reading an invite secret, proving a join. The passphrase that opens it has
//! so far come from `HRC_PASSPHRASE`, which works and is not somewhere a
//! passphrase should live — an environment variable is inherited by every
//! child process, readable from `/proc` on Linux by anything running as the
//! same user, and captured in shell history when it is set by hand.
//!
//! So it is asked for instead, in the pane, and used for that one invocation.
//! Nothing here stores it.
//!
//! This is a stopgap and worth naming as one. The right answer is the OS
//! keychain (decision DEC-051), which would remove the question entirely on
//! the platforms that have one; until that decision is taken, typing it is
//! better than exporting it.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What the human did with the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PassphraseOutcome {
    /// Use this passphrase for this invocation.
    Unlock(String),
    /// Leave without unlocking anything.
    Quit,
}

/// The passphrase prompt.
#[derive(Debug, Clone)]
pub struct PassphraseApp {
    entered: String,
    reason: String,
}

impl PassphraseApp {
    /// Builds the prompt, saying what it is about to be used for.
    ///
    /// A prompt that only says "passphrase:" asks a person to type a secret
    /// without telling them what will happen next, which is the shape of
    /// every credential-phishing screen ever built. Naming the operation is
    /// what makes it answerable.
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            entered: String::new(),
            reason: reason.into(),
        }
    }

    /// What the passphrase is being asked for.
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// The entry as it should be displayed.
    ///
    /// Asterisks, and the length is the only thing that leaks. Returning the
    /// entry itself would put a renderer one mistake away from painting a
    /// passphrase onto a shared screen.
    pub fn masked(&self) -> String {
        "*".repeat(self.entered.chars().count())
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<PassphraseOutcome> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(PassphraseOutcome::Quit);
        }

        match key.code {
            KeyCode::Esc => Some(PassphraseOutcome::Quit),
            KeyCode::Backspace => {
                self.entered.pop();
                None
            }
            // No confirmation step. Unlocking is not a decision with a
            // consequence to weigh — a wrong passphrase simply fails to open
            // the store — and a second press here would only train the habit
            // of confirming without reading.
            KeyCode::Enter => {
                (!self.entered.is_empty()).then(|| PassphraseOutcome::Unlock(self.entered.clone()))
            }
            KeyCode::Char(character) => {
                self.entered.push(character);
                None
            }
            _ => None,
        }
    }
}

impl crate::run::Screen for PassphraseApp {
    type Outcome = PassphraseOutcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_passphrase(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<PassphraseOutcome> {
        PassphraseApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
