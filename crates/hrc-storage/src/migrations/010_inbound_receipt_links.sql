-- Receipts occupy the same per-device chain as human messages, but never
-- belong in the inbox. Keep their authenticated links for ordering,
-- deduplication and fork detection without retaining a quarantined body.
CREATE TABLE inbound_receipt_link (
    message_id        TEXT PRIMARY KEY,
    channel_id        TEXT NOT NULL,
    sender_device     TEXT NOT NULL,
    device_sequence   INTEGER NOT NULL,
    chain_id          TEXT NOT NULL,
    previous_chain_id TEXT,
    ciphertext_sha256 TEXT NOT NULL,
    UNIQUE (channel_id, sender_device, device_sequence),
    UNIQUE (channel_id, chain_id),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE
) STRICT;
