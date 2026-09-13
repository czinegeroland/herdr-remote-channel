-- Initial local state for one HRC installation.
--
-- Scope note: this database holds local operational state only. It is not a
-- copy of the channel. Private keys live in the OS keychain and never appear
-- here (PRD section 14.1).

-- One row per channel this installation participates in.
CREATE TABLE channel (
    channel_id       TEXT PRIMARY KEY,
    transport_kind   TEXT NOT NULL,
    transport_locator TEXT NOT NULL,
    -- Local display name. Never sender-controlled: PRD section 19.1 keeps
    -- remote display strings out of pre-approval surfaces.
    local_name       TEXT NOT NULL,
    -- Roster epoch this installation has evaluated up to.
    roster_epoch     INTEGER NOT NULL DEFAULT 0,
    -- Last control sequence applied.
    control_sequence INTEGER NOT NULL DEFAULT 0,
    -- Durable synchronization cursor: the last transport revision fully
    -- processed. NULL before the first fetch (PRD section 12.5).
    sync_cursor      TEXT,
    -- Set when a tamper or history-rewrite condition halts synchronization.
    -- Sticky by design: it is cleared only by explicit human action.
    halted_reason    TEXT,
    created_at       TEXT NOT NULL
) STRICT;

-- The local device's per-channel send sequence.
--
-- Kept in its own table so allocation is a single UPDATE ... RETURNING under
-- the write lock, which is what makes concurrent allocation safe.
CREATE TABLE device_sequence (
    channel_id     TEXT NOT NULL,
    device_id      TEXT NOT NULL,
    -- Highest sequence allocated so far. The next allocation is this + 1.
    last_sequence  INTEGER NOT NULL DEFAULT 0,
    -- Chain ID of the last allocated message, or NULL for the first.
    last_chain_id  TEXT,
    PRIMARY KEY (channel_id, device_id),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE
) STRICT;

-- Messages this installation has created and not yet confirmed published.
--
-- PRD section 12.5 requires the outgoing object to be durable before any
-- publication attempt, so a crash cannot lose a message the user believes
-- was sent.
CREATE TABLE outbox (
    message_id      TEXT PRIMARY KEY,
    channel_id      TEXT NOT NULL,
    device_id       TEXT NOT NULL,
    device_sequence INTEGER NOT NULL,
    chain_id        TEXT NOT NULL,
    previous_chain_id TEXT,
    -- Roster epoch the payload was encrypted for. A later epoch change
    -- forces re-encryption before publication (PRD section 17.5).
    roster_epoch    INTEGER NOT NULL,
    -- Hash of the logical payload, stable across re-encryption.
    payload_hash    TEXT NOT NULL,
    -- Ciphertext, or NULL when the record is reserved but not yet built.
    ciphertext      BLOB,
    state           TEXT NOT NULL,
    attempts        INTEGER NOT NULL DEFAULT 0,
    last_error      TEXT,
    created_at      TEXT NOT NULL,
    updated_at      TEXT NOT NULL,
    UNIQUE (channel_id, device_id, device_sequence),
    UNIQUE (channel_id, chain_id),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE,
    CHECK (state IN ('reserved', 'queued', 'publishing', 'published', 'gap', 'failed'))
) STRICT;

CREATE INDEX outbox_pending ON outbox (channel_id, state, device_sequence);

-- Messages received from the transport.
--
-- A body arrives quarantined and stays that way until a human approves it
-- (PRD section 19.1). The agent-safe surface reads only the metadata columns
-- above the quarantine boundary.
CREATE TABLE inbox (
    message_id       TEXT PRIMARY KEY,
    channel_id       TEXT NOT NULL,
    -- Verified sender, resolved from the roster rather than from the object.
    sender_principal TEXT NOT NULL,
    sender_device    TEXT NOT NULL,
    -- Enumerated message kind. Unknown kinds are stored as 'unsupported'.
    kind             TEXT NOT NULL,
    thread_id        TEXT,
    in_reply_to      TEXT,
    roster_epoch     INTEGER NOT NULL,
    -- Validated endpoint identifier, or NULL. Never free remote text.
    endpoint         TEXT,
    ciphertext_bytes INTEGER NOT NULL,
    plaintext_bytes  INTEGER NOT NULL,
    created_at       TEXT NOT NULL,
    expires_at       TEXT,
    received_at      TEXT NOT NULL,
    -- Quarantined plaintext. Readable only through the trusted surface.
    body             BLOB,
    -- quarantined -> approved | edited | declined | expired | unsupported
    disposition      TEXT NOT NULL,
    read_at          TEXT,
    UNIQUE (channel_id, message_id),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE,
    CHECK (disposition IN ('quarantined', 'approved', 'edited', 'declined', 'expired', 'unsupported'))
) STRICT;

CREATE INDEX inbox_pending ON inbox (channel_id, disposition, received_at);
CREATE INDEX inbox_thread ON inbox (channel_id, thread_id);

-- Append-only record of local decisions (PRD sections 12.2 and 25).
--
-- There is no UPDATE or DELETE path for this table in the storage API. An
-- approval that cannot be reconstructed afterwards is not an audit trail.
CREATE TABLE audit (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    channel_id  TEXT,
    message_id  TEXT,
    -- What happened, for example 'approve_to_agent' or 'decline'.
    action      TEXT NOT NULL,
    -- Digest of the content the action applied to, so an approval is bound
    -- to what was actually approved.
    content_hash TEXT,
    -- Digest of edited content, when the human changed it before delivery.
    edited_hash TEXT,
    detail      TEXT,
    occurred_at TEXT NOT NULL
) STRICT;

CREATE INDEX audit_time ON audit (occurred_at);
