//! Transport adapter interface for Herdr Remote Channel.
//!
//! Owns the versioned JSON-RPC adapter contract, the capability
//! declaration, the linear append-only publication model, and the
//! conformance suite that every adapter must pass. Adapters move opaque
//! ciphertext and never receive plaintext or private keys.
//!
//! See PRD section 21. Implementation lands in milestone M2.
