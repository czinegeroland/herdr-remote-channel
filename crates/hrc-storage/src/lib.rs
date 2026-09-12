//! Durable local state for Herdr Remote Channel.
//!
//! Owns the SQLite database holding the inbox, outbox, quarantine, audit
//! log, and synchronization cursors, including the single transaction that
//! allocates a message ID, device sequence, predecessor chain ID, payload
//! hash, and outbox record together.
//!
//! See PRD sections 17.2 and 12.5. Implementation lands in milestone M1.
