//! Trusted inbox and approval terminal interface.
//!
//! This is the human-only surface of PRD sections 19.4 and 19.5: the one
//! place a quarantined body may be displayed, and the only way a decision
//! about it is made. It is unavailable to agent-safe RPC and to `--json`.
//!
//! The design constraint that shapes everything here is that a body is
//! *hidden until a human asks for it*, and a decision is never the default.
//! Section 19.4 requires manual approval, which means no key press should be
//! able to approve something by accident and no screen should ever open with
//! an approval already selected. So revealing takes a deliberate key, every
//! decision takes a second confirming key, and the confirmation names what is
//! about to happen.
//!
//! Rendering is separated from the terminal. [`App`] is a state machine over
//! key presses that yields [`Outcome`]s, and [`render`] draws it into any
//! ratatui buffer. That is what makes the interaction testable without a tty:
//! the tests drive real key events and read the real rendered buffer.

#![warn(missing_docs)]

pub mod app;
pub mod compose;
pub mod context;
pub mod join;
pub mod passphrase;
pub mod run;
pub mod setup;
pub mod view;

pub use app::{App, Focus, Outcome, PendingItem};
pub use compose::{ComposeApp, ComposeKind, ComposeOutcome, Recipient};
pub use context::{ContextApp, ContextDraft, ContextOutcome, ContextPreview};
pub use join::{JoinApp, JoinOutcome, PendingJoin};
pub use passphrase::{PassphraseApp, PassphraseOutcome};
pub use run::{Screen, run};
pub use setup::{SetupApp, SetupOutcome, SetupStep};
pub use view::{
    render, render_compose, render_context, render_joins, render_passphrase, render_setup,
};
