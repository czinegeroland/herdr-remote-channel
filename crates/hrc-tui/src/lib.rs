//! Trusted inbox and approval terminal interface.
//!
//! Owns the human-only surface that previews a quarantined body and issues
//! the one-use, digest-bound approval authorization consumed by the daemon.
//! This surface is unavailable to agent-safe RPC and to `--json` output.
//!
//! See PRD sections 19.4 and 19.5. Implementation lands in milestone M3.
