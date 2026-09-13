-- Approval decisions in the local audit log (PRD requirements HRC-GATE-004,
-- HRC-SEC-011, HRC-MSG-010).
--
-- The audit table already recorded that something happened. A decision needs
-- more than that: section 12.3 requires the original *and* edited content to
-- be preserved, because the question a reviewer asks later is not "was this
-- approved" but "what was approved, and what did the human change before it
-- reached an agent". A pair of digests cannot answer that.
--
-- This content is local and already stored quarantined in the inbox, so
-- keeping it here discloses nothing that the machine did not already hold.

ALTER TABLE audit ADD COLUMN original_content BLOB;
ALTER TABLE audit ADD COLUMN edited_content BLOB;

-- The local agent the human chose, never anything the sender asked for.
ALTER TABLE audit ADD COLUMN agent TEXT;

-- The local human who decided. An approval nobody is recorded as making is
-- not an audit trail.
ALTER TABLE audit ADD COLUMN decided_by TEXT;

CREATE INDEX audit_message ON audit (message_id, occurred_at);
