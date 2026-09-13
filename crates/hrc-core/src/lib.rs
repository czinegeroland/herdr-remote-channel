//! Identity, membership, messaging, and prompt gate core.
//!
//! Owns principal and device identities, control log validation, roster
//! epochs, message and thread state machines, context packages, the
//! quarantine boundary, and the audit record. It exposes an agent-safe
//! metadata surface separately from the trusted human content surface;
//! pending plaintext is never returned by the agent-safe surface.
//!
//! See PRD sections 13.1 and 19.

pub mod context;
pub mod delegation;
pub mod enrollment;
pub mod error;
pub mod gate;
pub mod message;
pub mod receipt;
pub mod roster;
pub mod rpc;
pub mod sync;

pub use context::{ContextItem, ContextPackage, ContextPreview};
pub use delegation::{Delegation, Party};
pub use enrollment::{PendingJoin, admit, request_join, review_join};
pub use error::{CoreError, Result};
pub use gate::{
    AgentView, Approval, Authorization, AuthorizationLedger, Decision, DecisionRecord, Delivery,
};
pub use message::{QuarantinedMessage, open, seal};
pub use receipt::{SentMessage, VerifiedReceipt, accept_receipt, correlate_answer};
pub use roster::{DeviceStatus, InviteState, Roster, RosterDevice, RosterMember};
pub use rpc::{AgentRequest, AgentResponse, Broker, Request, TrustedRequest, TrustedResponse};
