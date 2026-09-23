//! Context package drafts and received context (PRD section 20).

use super::*;

/// A context package received with an inbound message.
#[derive(Debug, Clone)]
pub struct InboundContext<'a> {
    /// Sender-selected package identifier.
    pub package_id: &'a str,
    /// Verified SHA-256 digest of the manifest.
    pub digest: &'a str,
    /// Canonical context manifest bytes.
    pub manifest: &'a [u8],
}

/// A locally drafted package ready to be checked and sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ContextDraft {
    /// Sender-selected package identifier.
    pub package_id: String,
    /// Repository used to evaluate ignore rules, when excerpts are present.
    pub repository_root: Option<String>,
    /// SHA-256 digest of the canonical manifest.
    pub digest: String,
    /// Canonical context manifest bytes.
    pub manifest: Vec<u8>,
}

/// Stores a verified context only after its owning inbox row exists.
///
/// The foreign key deliberately makes the message disposition its only
/// lifecycle: context cannot be approved, declined, or exposed separately.
pub(crate) fn store_inbound_context(
    transaction: &rusqlite::Transaction<'_>,
    message: &InboundMessage<'_>,
) -> Result<()> {
    if message.kind == "receipt" || message.expired {
        return Ok(());
    }
    let Some(context) = &message.context else {
        return Ok(());
    };
    transaction.execute(
        "INSERT INTO inbound_context (message_id, package_id, digest, manifest)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (message_id) DO NOTHING",
        params![
            message.message_id,
            context.package_id,
            context.digest,
            context.manifest,
        ],
    )?;
    Ok(())
}

impl Database {
    /// Stores a locally authored package without publishing it.
    pub fn save_context_draft(
        &self,
        package_id: &str,
        repository_root: Option<&str>,
        digest: &str,
        manifest: &[u8],
        now: &str,
    ) -> Result<()> {
        self.connection.execute(
            "INSERT INTO context_draft (package_id, repository_root, digest, manifest, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![package_id, repository_root, digest, manifest, now],
        )?;
        Ok(())
    }

    /// Returns one locally authored package.
    pub fn context_draft(&self, package_id: &str) -> Result<Option<ContextDraft>> {
        self.connection
            .query_row(
                "SELECT package_id, repository_root, digest, manifest
                 FROM context_draft WHERE package_id = ?1",
                params![package_id],
                |row| {
                    Ok(ContextDraft {
                        package_id: row.get(0)?,
                        repository_root: row.get(1)?,
                        digest: row.get(2)?,
                        manifest: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Every locally authored package, oldest first.
    ///
    /// Only the identifier and digest, not the manifest. A caller listing
    /// what could be sent has no business reading the contents of each one,
    /// and the manifests are large enough that loading them all to render a
    /// menu would be wasteful as well as wrong.
    pub fn context_drafts(&self) -> Result<Vec<(String, String)>> {
        let mut statement = self.connection.prepare(
            "SELECT package_id, digest FROM context_draft ORDER BY created_at, package_id",
        )?;

        let drafts = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<Vec<_>, _>>()?;

        Ok(drafts)
    }

    /// Reads quarantined context only for trusted code and tests.
    pub fn inbound_context(&self, message_id: &str) -> Result<Option<ContextDraft>> {
        self.connection
            .query_row(
                "SELECT context.package_id, NULL, context.digest, context.manifest
                 FROM inbound_context AS context
                 JOIN inbox ON inbox.message_id = context.message_id
                 WHERE context.message_id = ?1 AND inbox.disposition = 'quarantined'",
                params![message_id],
                |row| {
                    Ok(ContextDraft {
                        package_id: row.get(0)?,
                        repository_root: row.get(1)?,
                        digest: row.get(2)?,
                        manifest: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    /// Rejects one signed message whose attached context is malformed.
    ///
    /// This is sender-authored content, not a transport/history failure, so
    /// unrelated traffic must continue to synchronize.
    pub fn reject_malformed_context(
        &mut self,
        channel_id: &str,
        message_id: &str,
        reason: &str,
        now: &str,
    ) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let updated = transaction.execute(
            "UPDATE inbox SET disposition = 'unsupported'
             WHERE channel_id = ?1 AND message_id = ?2 AND disposition = 'quarantined'",
            params![channel_id, message_id],
        )?;
        if updated == 1 {
            transaction.execute(
                "INSERT INTO audit (
                     channel_id, message_id, action, content_hash, detail, occurred_at
                 ) VALUES (?1, ?2, 'malformed_context', NULL, ?3, ?4)",
                params![channel_id, message_id, reason, now],
            )?;
        }
        transaction.commit()?;
        Ok(())
    }
}
