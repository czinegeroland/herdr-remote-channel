//! One conversation, read top to bottom (docs/RESEARCH.md 6.4).
//!
//! Opened zoomed, because a conversation is the one thing that wants the
//! whole screen and wants to stay open while someone reads it. It is
//! read-only by construction: the only keys move the view or leave it, and
//! the only outcome is leaving. What each entry says is decided by the
//! caller, which holds the rule that matters -- content appears only where a
//! human already released it -- so this screen cannot become a second way
//! to read a quarantined body.

use std::cell::Cell;

use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

/// One message in the thread, as it should be shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadEntry {
    /// Who and what, in locally resolved words: `alice · question`.
    pub heading: String,
    /// Released content, or a fixed local line saying why there is none.
    pub body: String,
}

/// The thread screen.
#[derive(Debug, Clone)]
pub struct ThreadApp {
    title: String,
    entries: Vec<ThreadEntry>,
    scroll: Cell<u16>,
    max_scroll: Cell<u16>,
    page: Cell<u16>,
}

impl ThreadApp {
    /// Builds the screen over entries already in reading order.
    pub fn new(title: impl Into<String>, entries: Vec<ThreadEntry>) -> Self {
        Self {
            title: title.into(),
            entries,
            scroll: Cell::new(0),
            max_scroll: Cell::new(0),
            page: Cell::new(1),
        }
    }

    /// The title the frame carries.
    pub fn title(&self) -> &str {
        &self.title
    }

    /// The entries, in reading order.
    pub fn entries(&self) -> &[ThreadEntry] {
        &self.entries
    }

    /// The first rendered row currently shown.
    pub fn scroll(&self) -> u16 {
        self.scroll.get()
    }

    /// Records the rendered size and clamps the scroll to it.
    pub(crate) fn set_viewport(&self, total_rows: usize, visible_rows: u16) {
        let max = total_rows
            .saturating_sub(visible_rows as usize)
            .min(u16::MAX as usize) as u16;
        self.page.set(visible_rows.max(1));
        self.max_scroll.set(max);
        self.scroll.set(self.scroll.get().min(max));
    }

    /// Handles one key. `Some(())` means leave.
    pub fn on_key(&mut self, key: KeyEvent) -> Option<()> {
        if key.kind != KeyEventKind::Press {
            return None;
        }

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Some(());
        }

        let scroll = self.scroll.get();
        let page = self.page.get();
        let next = match key.code {
            KeyCode::Esc | KeyCode::Char('q') => return Some(()),
            KeyCode::Down | KeyCode::Char('j') => scroll.saturating_add(1),
            KeyCode::Up | KeyCode::Char('k') => scroll.saturating_sub(1),
            KeyCode::PageDown | KeyCode::Char(' ') => scroll.saturating_add(page),
            KeyCode::PageUp => scroll.saturating_sub(page),
            KeyCode::Home | KeyCode::Char('g') => 0,
            KeyCode::End | KeyCode::Char('G') => self.max_scroll.get(),
            _ => return None,
        };
        self.scroll.set(next.min(self.max_scroll.get()));
        None
    }
}

impl crate::run::Screen for ThreadApp {
    type Outcome = ();

    fn draw(&self, frame: &mut ratatui::Frame<'_>) {
        crate::view::render_thread(frame, self);
    }

    fn on_key(&mut self, key: KeyEvent) -> Option<()> {
        ThreadApp::on_key(self, key)
    }
}

#[cfg(test)]
mod tests;
