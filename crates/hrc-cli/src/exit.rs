//! Documented exit codes.
//!
//! PRD section 11.1 requires stable, documented exit codes for every
//! non-interactive command. Callers, including the Herdr plugin and the
//! agent skill, branch on these numbers, so an assigned value never changes
//! meaning. New conditions take a new number.
//!
//! `0` is success and `1` is a generic runtime failure. Neither has a
//! constant yet because no command path produces them; they are reserved.

/// The command line was invalid, or an option is not supported for this
/// command. Clap also uses this code for its own parse errors.
pub const USAGE: i32 = 2;

/// The command exists in the published contract but its behavior has not
/// shipped yet.
pub const UNIMPLEMENTED: i32 = 3;

/// The operation crosses the human authorization boundary of PRD section
/// 22.7 and must be completed in the trusted local interface. Agent-safe and
/// non-interactive callers always receive this code.
pub const AUTHORIZATION_REQUIRED: i32 = 4;
