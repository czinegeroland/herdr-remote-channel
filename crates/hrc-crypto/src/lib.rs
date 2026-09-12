//! Cryptographic operations for Herdr Remote Channel.
//!
//! Owns Ed25519 signing and verification, age encryption to explicit
//! recipient devices, SHA-256 object hashing, invite proofs, and safety
//! phrase derivation. This crate never introduces custom primitives and
//! never exposes private key material through a public return value.
//!
//! See PRD section 14. Implementation lands in milestone M1.
