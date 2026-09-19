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
pub mod inbox;
pub mod join;
pub mod members;
pub mod passphrase;
pub mod run;
pub mod setup;
pub mod view;

// Re-exported so a [`Screen`] can be implemented outside this crate. The
// trait's methods name a `KeyEvent` and a `Frame`, and a crate that could not
// name those types could not implement it — which would push every screen
// back in here, including the ones that need a database handle this crate
// deliberately does not have.
pub use crossterm;
pub use ratatui;

pub use app::{App, Focus, Outcome, PendingItem};
pub use compose::{ComposeApp, ComposeKind, ComposeOutcome, Recipient};
pub use context::{ContextApp, ContextDraft, ContextOutcome, ContextPreview};
pub use inbox::{InboxApp, InboxFilter, InboxOutcome};
pub use join::{JoinApp, JoinOutcome, PendingJoin};
pub use members::{Member, MemberDevice, MemberOutcome, MembersApp};
pub use passphrase::{PassphraseApp, PassphraseOutcome};
pub use run::{Screen, Ticking, run, run_ticking};
pub use setup::{SetupApp, SetupOutcome, SetupStep};
pub use view::{
    render, render_compose, render_context, render_inbox, render_joins, render_members,
    render_passphrase, render_setup,
};
