-- Recipient-scoped message ordering and receive progress.
--
-- The original inbound chain is intentionally retained for legacy envelopes.
-- New envelopes carry one predecessor per intended recipient device and use
-- these separate tables, so upgrading cannot reinterpret an existing legacy
-- head or manufacture continuity that was never observed.

ALTER TABLE channel ADD COLUMN receive_cursor TEXT;

-- Resealing an earlier message can change the predecessor map required by
-- later queued ciphertext even when those later rows already use the current
-- roster epoch.
ALTER TABLE outbox ADD COLUMN recipient_order_stale INTEGER NOT NULL DEFAULT 0
    CHECK (recipient_order_stale IN (0, 1));

CREATE TABLE inbound_recipient_chain (
    channel_id       TEXT NOT NULL,
    sender_device    TEXT NOT NULL,
    recipient_device TEXT NOT NULL,
    last_sequence    INTEGER NOT NULL,
    last_chain_id    TEXT NOT NULL,
    PRIMARY KEY (channel_id, sender_device, recipient_device),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE
) STRICT;

-- One authenticated identity/order record for every recipient-ordered
-- message, including receipts and expired messages that never become
-- actionable inbox entries.
CREATE TABLE inbound_recipient_link (
    message_id        TEXT PRIMARY KEY,
    channel_id        TEXT NOT NULL,
    sender_device     TEXT NOT NULL,
    recipient_device  TEXT NOT NULL,
    device_sequence   INTEGER NOT NULL,
    chain_id          TEXT NOT NULL,
    previous_chain_id TEXT,
    ciphertext_sha256 TEXT NOT NULL,
    state             TEXT NOT NULL,
    is_receipt        INTEGER NOT NULL,
    is_expired        INTEGER NOT NULL,
    UNIQUE (channel_id, sender_device, device_sequence),
    UNIQUE (channel_id, chain_id),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE,
    CHECK (state IN ('held', 'accepted')),
    CHECK (is_receipt IN (0, 1)),
    CHECK (is_expired IN (0, 1))
) STRICT;

-- One predecessor can have only one successor within a sender/recipient
-- scope. COALESCE makes the initial NULL predecessor participate too.
CREATE UNIQUE INDEX inbound_recipient_successor
    ON inbound_recipient_link (
        channel_id,
        sender_device,
        recipient_device,
        COALESCE(previous_chain_id, '')
    );

CREATE INDEX inbound_recipient_held
    ON inbound_recipient_link (
        channel_id,
        sender_device,
        recipient_device,
        state,
        previous_chain_id
    );

CREATE INDEX outbox_recipient_predecessor
    ON outbox_recipient (device_id, message_id);
