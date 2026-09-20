//! The composition screen.
//!
//! Sending is not a trusted-human operation — an agent may draft and a
//! script may send, which is why `hrc send` works from an ordinary shell.
//! This screen exists for a different reason: so that a person using Herdr
//! never has to leave it to say something.
//!
//! It still asks twice before sending. Not because sending is dangerous in
//! the way disclosure is, but because a message is published to an
//! append-only history that no one can edit afterwards, and the recipient is
//! chosen from a list where two entries may differ by a few characters of
//! base64. The confirmation names the recipient for exactly that reason.

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// What kind of message is being written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeKind {
    /// An informational note.
    Note,
    /// A question, which asks the recipient for an answer.
    Question,
}

impl ComposeKind {
    /// Every kind this screen offers.
    pub const ALL: [ComposeKind; 2] = [ComposeKind::Note, ComposeKind::Question];

    /// The name used on the wire and in the CLI.
    pub const fn as_str(self) -> &'static str {
        match self {
            ComposeKind::Note => "note",
            ComposeKind::Question => "question",
        }
    }

    /// The next kind, wrapping.
    const fn next(self) -> ComposeKind {
        match self {
            ComposeKind::Note => ComposeKind::Question,
            ComposeKind::Question => ComposeKind::Note,
        }
    }
}

/// Someone this installation can address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Recipient {
    /// The verified principal.
    pub principal_id: String,
    /// What this installation calls them, if anything.
    ///
    /// Addressing someone by a base64 principal is the same problem the
    /// inbox had: the one field a person needs to read is the one they
    /// cannot. `None` falls back to a shortened principal rather than to
    /// nothing, because a recipient with no label at all could not be
    /// chosen.
    pub display_name: Option<String>,
    /// Whether the roster still lists them as active.
    pub active: bool,
    /// The message this would answer, when the target is a reply.
    ///
    /// A reply is not a separate screen, because it is the same act: text to
    /// one person. What differs is that the thread is read from local state
    /// rather than started fresh — a sender that could choose its own thread
    /// could attach a reply to any conversation. So the choice is in this
    /// list, and picking it fixes the recipient too.
    pub in_reply_to: Option<String>,
    /// A short description of what is being answered, for the list.
    pub answering: Option<String>,
}

/// Which part of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeFocus {
    /// Choosing who it goes to.
    Recipient,
    /// Writing the message.
    Body,
}

/// What the human asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeOutcome {
    /// Publish this message.
    Send {
        /// Who it goes to.
        recipient: String,
        /// What kind it is.
        kind: ComposeKind,
        /// The text.
        text: String,
        /// The message being answered, when this is a reply.
        in_reply_to: Option<String>,
    },
    /// Leave without sending.
    Quit,
}

/// The composition screen.
#[derive(Debug, Clone)]
pub struct ComposeApp {
    recipients: Vec<Recipient>,
    selected: usize,
    kind: ComposeKind,
    body: String,
    focus: ComposeFocus,
    proposed: Option<String>,
    status: String,
}

impl ComposeApp {
    /// Builds the screen over the people this installation can write to.
    ///
    /// The local principal is expected to have been filtered out already:
    /// the roster contains it, and offering it would make "send to myself"
    /// the first entry on a list where the first entry is preselected.
    pub fn new(recipients: Vec<Recipient>) -> Self {
        Self {
            recipients,
            selected: 0,
            kind: ComposeKind::Note,
            body: String::new(),
            focus: ComposeFocus::Recipient,
            proposed: None,
            status: "Tab: write   k: note or question   Ctrl+S: send   Esc: leave".into(),
        }
    }

    /// The people on screen.
    pub fn recipients(&self) -> &[Recipient] {
        &self.recipients
    }

    /// The selected recipient, if there is one.
    pub fn selected(&self) -> Option<&Recipient> {
        self.recipients.get(self.selected)
    }

    /// The index of the selection.
    pub fn selected_index(&self) -> usize {
        self.selected
    }

    /// The kind being written.
    pub fn kind(&self) -> ComposeKind {
        self.kind
    }

    /// The text written so far.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// Where the keyboard is.
    pub fn focus(&self) -> ComposeFocus {
        self.focus
    }

    /// The send awaiting confirmation, if any.
    pub fn proposed(&self) -> Option<&str> {
        self.proposed.as_deref()
    }

    /// The line telling the human what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<ComposeOutcome> {
        // The same hazard as every other screen here: Windows reports a
        // press and a release for one physical key, and counting both would
        // double every character typed as well as halving the confirmation.
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(ComposeOutcome::Quit);
        }

        if self.proposed.is_some() {
            return self.confirm(key);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.propose();
            return None;
        }

        match self.focus {
            ComposeFocus::Recipient => self.on_recipient_key(key),
            ComposeFocus::Body => self.on_body_key(key),
        }
    }

    /// Keys while choosing a recipient.
    fn on_recipient_key(&mut self, key: KeyEvent) -> Option<ComposeOutcome> {
        match key.code {
            KeyCode::Esc => Some(ComposeOutcome::Quit),
            KeyCode::Tab => {
                self.focus = ComposeFocus::Body;
                self.status = "Writing. Tab: back to recipients   Ctrl+S: send".into();
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
            // `k` moves up, so the kind toggle is on a key that cannot also
            // be a movement: `t` for type.
            KeyCode::Char('t') => {
                self.kind = self.kind.next();
                None
            }
            _ => None,
        }
    }

    /// Keys while writing the message.
    ///
    /// Escape returns to the recipient list rather than leaving the screen.
    /// Losing a half-written message to one key press is the kind of thing
    /// that teaches people to compose somewhere else and paste it in.
    fn on_body_key(&mut self, key: KeyEvent) -> Option<ComposeOutcome> {
        match key.code {
            KeyCode::Esc | KeyCode::Tab => {
                self.focus = ComposeFocus::Recipient;
                self.status = "Tab: write   t: note or question   Ctrl+S: send   Esc: leave".into();
                None
            }
            KeyCode::Backspace => {
                self.body.pop();
                None
            }
            KeyCode::Enter => {
                self.body.push('\n');
                None
            }
            KeyCode::Char(character) => {
                self.body.push(character);
                None
            }
            _ => None,
        }
    }

    /// Answers the confirmation. Only `y` sends.
    fn confirm(&mut self, key: KeyEvent) -> Option<ComposeOutcome> {
        let _ = self.proposed.take();

        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.status = "Cancelled. Nothing was sent.".into();
            return None;
        }

        let recipient = self.recipients.get(self.selected)?;
        Some(ComposeOutcome::Send {
            recipient: recipient.principal_id.clone(),
            kind: self.kind,
            text: self.body.clone(),
            in_reply_to: recipient.in_reply_to.clone(),
        })
    }

    /// Moves the selection.
    fn move_selection(&mut self, delta: isize) {
        if self.recipients.is_empty() {
            return;
        }

        let last = self.recipients.len() - 1;
        self.selected = match delta {
            d if d < 0 => self.selected.saturating_sub(1),
            _ => (self.selected + 1).min(last),
        };
    }

    /// Proposes sending, or explains why it cannot.
    fn propose(&mut self) {
        let Some(recipient) = self.recipients.get(self.selected) else {
            self.status = "Nobody to send to yet. Invite someone first.".into();
            return;
        };

        // An empty message is almost always a mis-keyed send rather than an
        // intended one, and it costs the recipient a decision either way.
        if self.body.trim().is_empty() {
            self.status = "Nothing written yet.".into();
            return;
        }

        if !recipient.active {
            self.status =
                "That member is no longer active in this channel. Pick someone else.".into();
            return;
        }

        // The recipient is named in full. Two principals can differ by a few
        // characters of base64, and this is the last point at which a human
        // can notice they picked the wrong one.
        self.proposed = Some(match &recipient.answering {
            // An answer says which conversation it joins, because that is the
            // part a person cannot verify afterwards by reading their own
            // text back.
            Some(answering) => format!("Reply to {} ({answering})? [y/N]", recipient.principal_id),
            None => format!(
                "Send this {} to {}? [y/N]",
                self.kind.as_str(),
                recipient.principal_id
            ),
        });
    }
}

impl crate::run::Screen for ComposeApp {
    type Outcome = ComposeOutcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_compose(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<ComposeOutcome> {
        ComposeApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
