//! The context disclosure screen.
//!
//! PRD section 20 packages are the one thing this product sends that is not a
//! message someone typed: they carry file excerpts, diffs and command output
//! from a real repository. Section 22.4 puts both preview and send behind the
//! human authorization boundary for that reason, and section 22.7 says the
//! trusted interface performs the operation itself rather than handing back a
//! plan for someone else to carry out.
//!
//! So this screen shows what would leave the machine and then sends it. The
//! authorization is issued and consumed inside one trusted call (decision
//! DEC-040) and never reaches this code at all — there is no value here that
//! could be stored or replayed.
//!
//! Two rules shape the display. A package that is not sendable cannot be
//! confirmed, because a secret finding or an excluded path is the whole
//! reason preview exists. And the digest is shown, because the thing being
//! authorized is a specific set of bytes rather than a package name that
//! could be re-drafted with different contents afterwards.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One locally drafted package that could be disclosed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDraft {
    /// Sender-chosen package identifier.
    pub package_id: String,
    /// Digest of the canonical manifest.
    pub digest: String,
}

/// What the trusted preview reported about a package.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContextPreview {
    /// Digest of the exact canonical package shown to the human.
    pub digest: String,
    /// Exact canonical package content that would leave the machine.
    pub content: String,
    /// Item kinds and their byte counts.
    pub items: Vec<(String, u64)>,
    /// Total bytes the package would disclose.
    pub total_bytes: u64,
    /// Secret-scan findings, each one blocking.
    pub secrets: Vec<String>,
    /// Paths the repository's own rules exclude, each one blocking.
    pub excluded: Vec<String>,
    /// Whether the daemon considers it sendable.
    pub sendable: bool,
}

/// Which part of the screen has the keyboard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContextFocus {
    /// Choosing which package.
    Package,
    /// Choosing who it goes to.
    Recipient,
    /// Reading what the preview reported.
    Preview,
}

/// What the human asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContextOutcome {
    /// Ask the daemon what this package would disclose.
    Preview {
        /// Which package.
        package_id: String,
        /// Who it would go to; the authorization binds to them.
        recipient: String,
    },
    /// Disclose it.
    Send {
        /// Which package.
        package_id: String,
        /// Who it goes to.
        recipient: String,
    },
    /// Leave without disclosing anything.
    Quit,
}

/// The context disclosure screen.
#[derive(Debug, Clone)]
pub struct ContextApp {
    drafts: Vec<ContextDraft>,
    recipients: Vec<String>,
    selected_draft: usize,
    selected_recipient: usize,
    focus: ContextFocus,
    preview: Option<ContextPreview>,
    proposed: Option<String>,
    preview_scroll: Cell<u16>,
    preview_page_rows: Cell<u16>,
    preview_max_scroll: Cell<u16>,
    status: String,
}

impl ContextApp {
    /// Builds the screen over what this installation could disclose, and to
    /// whom.
    pub fn new(drafts: Vec<ContextDraft>, recipients: Vec<String>) -> Self {
        Self {
            drafts,
            recipients,
            selected_draft: 0,
            selected_recipient: 0,
            focus: ContextFocus::Package,
            preview: None,
            proposed: None,
            preview_scroll: Cell::new(0),
            preview_page_rows: Cell::new(1),
            preview_max_scroll: Cell::new(0),
            status: "Tab: choose recipient   Enter: see what would be sent   Esc: leave".into(),
        }
    }

    /// The packages on screen.
    pub fn drafts(&self) -> &[ContextDraft] {
        &self.drafts
    }

    /// The people this could go to.
    pub fn recipients(&self) -> &[String] {
        &self.recipients
    }

    /// The selected package, if there is one.
    pub fn selected_draft(&self) -> Option<&ContextDraft> {
        self.drafts.get(self.selected_draft)
    }

    /// The selected recipient, if there is one.
    pub fn selected_recipient(&self) -> Option<&String> {
        self.recipients.get(self.selected_recipient)
    }

    /// The index of the selected package.
    pub fn selected_draft_index(&self) -> usize {
        self.selected_draft
    }

    /// The index of the selected recipient.
    pub fn selected_recipient_index(&self) -> usize {
        self.selected_recipient
    }

    /// Where the keyboard is.
    pub fn focus(&self) -> ContextFocus {
        self.focus
    }

    /// What the daemon reported, once it has been asked.
    pub fn preview(&self) -> Option<&ContextPreview> {
        self.preview.as_ref()
    }

    /// The send awaiting confirmation, if any.
    pub fn proposed(&self) -> Option<&str> {
        self.proposed.as_deref()
    }

    /// The line telling the human what to do next.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// The first wrapped preview row currently shown.
    pub fn preview_scroll(&self) -> u16 {
        self.preview_scroll.get()
    }

    /// Updates the rendered preview dimensions and clamps scrolling after resize.
    pub(crate) fn set_preview_viewport(&self, total_rows: usize, visible_rows: u16) {
        let max_scroll = total_rows
            .saturating_sub(visible_rows as usize)
            .min(u16::MAX as usize) as u16;
        self.preview_page_rows.set(visible_rows.max(1));
        self.preview_max_scroll.set(max_scroll);
        self.preview_scroll
            .set(self.preview_scroll.get().min(max_scroll));
    }

    /// Records what the daemon's trusted preview reported.
    ///
    /// Moving the selection afterwards discards it: a preview describes one
    /// package for one recipient, and showing it beside a different selection
    /// would invite a decision about the wrong bytes.
    pub fn show_preview(&mut self, preview: ContextPreview) {
        self.status = if preview.sendable {
            "j/k/PgUp/PgDn: scroll   Up/Down: select   Ctrl+S: send   Esc: back".into()
        } else {
            "This package cannot be sent. Findings must be resolved in the draft first.".into()
        };

        self.preview = Some(preview);
        self.preview_scroll.set(0);
        self.focus = ContextFocus::Preview;
    }

    /// Handles one key press.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<ContextOutcome> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(ContextOutcome::Quit);
        }

        if self.proposed.is_some() {
            return self.confirm(key);
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('s') {
            self.propose();
            return None;
        }

        match key.code {
            KeyCode::Esc => {
                if self.preview.is_some() {
                    // Back to the selection rather than out of the screen, so
                    // leaving takes a second deliberate press.
                    self.preview = None;
                    self.preview_scroll.set(0);
                    self.focus = ContextFocus::Package;
                    self.status =
                        "Tab: choose recipient   Enter: see what would be sent   Esc: leave".into();
                    None
                } else {
                    Some(ContextOutcome::Quit)
                }
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    ContextFocus::Package => ContextFocus::Recipient,
                    _ => ContextFocus::Package,
                };
                self.preview = None;
                self.preview_scroll.set(0);
                None
            }
            KeyCode::Char('j') if self.focus == ContextFocus::Preview => {
                self.scroll_preview(1);
                None
            }
            KeyCode::Char('k') if self.focus == ContextFocus::Preview => {
                self.scroll_preview(-1);
                None
            }
            KeyCode::PageDown if self.focus == ContextFocus::Preview => {
                self.scroll_preview(i32::from(self.preview_page_rows.get()));
                None
            }
            KeyCode::PageUp if self.focus == ContextFocus::Preview => {
                self.scroll_preview(-i32::from(self.preview_page_rows.get()));
                None
            }
            KeyCode::Home if self.focus == ContextFocus::Preview => {
                self.preview_scroll.set(0);
                None
            }
            KeyCode::End if self.focus == ContextFocus::Preview => {
                self.preview_scroll.set(self.preview_max_scroll.get());
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
            KeyCode::Enter => self.request_preview(),
            _ => None,
        }
    }

    /// Asks for the trusted preview of the current selection.
    fn request_preview(&mut self) -> Option<ContextOutcome> {
        let package_id = self.selected_draft()?.package_id.clone();
        let recipient = self.selected_recipient()?.clone();

        Some(ContextOutcome::Preview {
            package_id,
            recipient,
        })
    }

    /// Answers the confirmation. Only `y` discloses.
    fn confirm(&mut self, key: KeyEvent) -> Option<ContextOutcome> {
        let _ = self.proposed.take();

        if !matches!(key.code, KeyCode::Char('y') | KeyCode::Char('Y')) {
            self.status = "Cancelled. Nothing was sent.".into();
            return None;
        }

        Some(ContextOutcome::Send {
            package_id: self.selected_draft()?.package_id.clone(),
            recipient: self.selected_recipient()?.clone(),
        })
    }

    /// Moves whichever list has the keyboard.
    fn move_selection(&mut self, delta: isize) {
        let left_preview = self.focus == ContextFocus::Preview;
        let (index, length) = match self.focus {
            ContextFocus::Recipient => (&mut self.selected_recipient, self.recipients.len()),
            _ => (&mut self.selected_draft, self.drafts.len()),
        };

        if length == 0 {
            return;
        }

        *index = match delta {
            d if d < 0 => index.saturating_sub(1),
            _ => (*index + 1).min(length - 1),
        };

        // The preview described the previous selection.
        self.preview = None;
        self.preview_scroll.set(0);
        if left_preview {
            self.focus = ContextFocus::Package;
        }
    }

    /// Scrolls the canonical preview without changing package or recipient.
    fn scroll_preview(&self, delta: i32) {
        let current = i32::from(self.preview_scroll.get());
        let maximum = i32::from(self.preview_max_scroll.get());
        self.preview_scroll
            .set((current + delta).clamp(0, maximum) as u16);
    }

    /// Proposes disclosing the previewed package.
    fn propose(&mut self) {
        let Some(preview) = &self.preview else {
            self.status = "Press Enter first to see what would be sent.".into();
            return;
        };

        // A package with findings is refused here as well as by the daemon.
        // The daemon's refusal is the one that matters; this one is so the
        // human is told why rather than watching a confirmation fail.
        if !preview.sendable {
            self.status =
                "This package cannot be sent. Findings must be resolved in the draft first.".into();
            return;
        }

        let (Some(draft), Some(recipient)) = (self.selected_draft(), self.selected_recipient())
        else {
            return;
        };

        // The digest is named because the thing being authorized is a
        // specific set of bytes, not a package name that could be re-drafted
        // with different contents afterwards.
        self.proposed = Some(format!(
            "Send {} bytes to {}? Package {} digest {}. [y/N]",
            preview.total_bytes,
            recipient,
            draft.package_id,
            &preview.digest[..preview.digest.len().min(16)]
        ));
    }
}

impl crate::run::Screen for ContextApp {
    type Outcome = ContextOutcome;

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_context(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<ContextOutcome> {
        ContextApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
