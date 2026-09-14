//! Running the approval screen on a real terminal.
//!
//! [`crate::App`] is a state machine and [`crate::render`] draws it; this is
//! the only part that touches a tty. Keeping it separate is what lets the
//! interaction be tested by driving real key events against a real rendered
//! buffer, with no terminal involved.
//!
//! The terminal is left exactly as it was found, including when the body of
//! the loop fails. A trusted approval screen that exits leaving the terminal
//! in raw mode with the alternate screen still active has, from the human's
//! point of view, broken their shell — and it would do so at the moment they
//! were deciding whether to disclose something.

use std::io::{self, Stdout};

use crossterm::{
    event::{self, Event},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::{App, Outcome, render};

/// Restores the terminal when it goes out of scope.
///
/// A guard rather than a pair of calls around the loop, so an error or a
/// panic inside the loop still puts the terminal back. `Drop` runs during
/// unwinding; an early `return` would not.
struct TerminalGuard {
    terminal: Terminal<CrosstermBackend<Stdout>>,
}

impl TerminalGuard {
    /// Takes the terminal into raw mode on the alternate screen.
    fn enter() -> io::Result<Self> {
        enable_raw_mode()?;
        let mut stdout = io::stdout();
        if let Err(error) = execute!(stdout, EnterAlternateScreen) {
            // Raw mode is already on at this point, so it has to come back
            // off before the error leaves.
            let _ = disable_raw_mode();
            return Err(error);
        }

        Ok(Self {
            terminal: Terminal::new(CrosstermBackend::new(stdout))?,
        })
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Every failure here is ignored deliberately. This runs while the
        // process is already leaving, possibly while unwinding, and there is
        // nowhere useful for an error to go; trying harder than this would
        // risk panicking inside a panic.
        let _ = disable_raw_mode();
        let _ = execute!(self.terminal.backend_mut(), LeaveAlternateScreen);
        let _ = self.terminal.show_cursor();
    }
}

/// Shows the approval screen until the human decides or leaves.
///
/// Returns what they chose. Carrying it out is the caller's job: the screen
/// reports a decision and the daemon's trusted path applies it, so this
/// cannot become a second place where approval rules live.
pub fn run(app: &mut App) -> io::Result<Outcome> {
    let mut guard = TerminalGuard::enter()?;

    loop {
        guard.terminal.draw(|frame| render(frame, app))?;

        // Only key events matter, and only some of them. A resize redraws on
        // the next pass, which the loop does anyway.
        if let Event::Key(key) = event::read()?
            && let Some(outcome) = app.on_key(key)
        {
            return Ok(outcome);
        }
    }
}
