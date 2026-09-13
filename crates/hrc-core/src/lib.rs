//! Identity, membership, messaging, and prompt gate core.
//!
//! Owns principal and device identities, control log validation, roster
//! epochs, message and thread state machines, context packages, the
//! quarantine boundary, and the audit record. It exposes an agent-safe
//! metadata surface separately from the trusted human content surface;
//! pending plaintext is never returned by the agent-safe surface.
//!
//! See PRD sections 13.1 and 19.

pub mod error;
pub mod message;
pub mod roster;

pub use error::{CoreError, Result};
pub use message::{QuarantinedMessage, open, seal};
pub use roster::{DeviceStatus, Roster, RosterDevice, RosterMember};
