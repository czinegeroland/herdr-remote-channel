//! Database schema and migrations.
//!
//! The schema is versioned through SQLite's `user_version` pragma and applied
//! in order inside one transaction, so a database is never left half-migrated
//! by a crash. Migrations are append-only: an applied migration is never
//! edited, because an existing database would not re-run it.

use rusqlite::Connection;

use crate::error::{Result, StorageError};

/// The schema version this build expects.
pub const SCHEMA_VERSION: u32 = 3;

/// Migrations in order. Index zero moves version 0 to version 1.
const MIGRATIONS: &[&str] = &[
    include_str!("migrations/001_initial.sql"),
    include_str!("migrations/002_inbound_order.sql"),
    include_str!("migrations/003_receipts.sql"),
];

/// Brings a connection up to [`SCHEMA_VERSION`].
///
/// A database newer than this build is an error rather than something to
/// work around: an older binary cannot know what a newer schema means, and
/// guessing risks corrupting state a newer binary depends on.
pub fn migrate(connection: &mut Connection) -> Result<()> {
    let current: u32 =
        connection.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))? as u32;

    if current > SCHEMA_VERSION {
        return Err(StorageError::SchemaTooNew {
            found: current,
            supported: SCHEMA_VERSION,
        });
    }

    if current == SCHEMA_VERSION {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    for (index, migration) in MIGRATIONS.iter().enumerate().skip(current as usize) {
        transaction.execute_batch(migration)?;
        let version = index as u32 + 1;
        // `pragma_update` does not accept a bound parameter for the value.
        transaction.pragma_update(None, "user_version", version)?;
    }
    transaction.commit()?;

    Ok(())
}
