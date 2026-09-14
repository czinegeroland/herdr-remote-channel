//! Herdr plugin integration for Herdr Remote Channel.
//!
//! Owns the Herdr startup, action, event, and pane entry points that the
//! single `hrc` executable dispatches (PRD requirement HRC-TECH-002), plus
//! the sidebar indicator, the inbox view, the notification set, and the
//! selection of a local agent for approved prompt delivery.
//!
//! Everything here is a pure function of state the caller passes in. The
//! plugin does not open sockets, read the database, or decrypt anything: the
//! `hrc` binary talks to the daemon and hands the answers to this crate,
//! which decides what the human sees. That split is what makes the plugin's
//! behavior testable without a daemon, and it keeps the one rule this crate
//! must never break — a pending body is never rendered on a surface an agent
//! can read — expressible as a property of types rather than of a process.
//!
//! Two constraints from the PRD shape the API and are worth stating before
//! the code implies them:
//!
//! * **Remote participants never receive local pane or agent identifiers.**
//!   PRD section 4 constraint 8 and section 23.4. Local agent identity lives
//!   in [`agent::LocalAgent`], which is deliberately not serializable to
//!   anything that crosses the channel.
//! * **The inbox view carries only the closed metadata set of section
//!   19.1.** Rows are built from [`hrc_core::gate::AgentView`], which already
//!   substitutes fixed local labels for sender-chosen text.
//!
//! See PRD section 23.

#![warn(missing_docs)]

pub mod agent;
pub mod event;
pub mod inbox;
pub mod manifest;
pub mod notify;
pub mod pane;
pub mod sidebar;

pub use agent::LocalAgent;
pub use event::{Event, Reaction};
pub use inbox::{InboxRow, InboxView, agent_view};
pub use manifest::manifest;
pub use notify::Notification;
pub use pane::Pane;
pub use sidebar::Sidebar;
