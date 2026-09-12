//! Herdr plugin integration for Herdr Remote Channel.
//!
//! Owns the Herdr startup, action, event, and pane entry points that the
//! single `hrc` executable dispatches, plus sidebar indicators and the
//! selection of a local agent for approved prompt delivery. Remote
//! participants never receive local pane IDs.
//!
//! See PRD section 23. Implementation lands in milestone M3.
