-- Locally authored packages and remote packages held behind the same
-- quarantine boundary as inbound message bodies.
CREATE TABLE context_draft (
    package_id  TEXT PRIMARY KEY,
    -- Canonical absolute Git worktree root, never a caller-provided label.
    repository_root TEXT,
    digest      TEXT NOT NULL,
    manifest    BLOB NOT NULL,
    created_at  TEXT NOT NULL
) STRICT;

CREATE TABLE inbound_context (
    message_id  TEXT PRIMARY KEY,
    package_id  TEXT NOT NULL,
    digest      TEXT NOT NULL,
    manifest    BLOB NOT NULL,
    -- Inbox owns the context lifecycle; it cannot drift separately.
    FOREIGN KEY (message_id) REFERENCES inbox (message_id) ON DELETE CASCADE
) STRICT;
