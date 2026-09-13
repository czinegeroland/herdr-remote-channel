-- Per-device inbound ordering and deduplication (PRD sections 18.1, 12.2).
--
-- The initial inbox recorded what arrived. It could not answer two questions
-- the protocol requires: has this message already been accepted, and is it
-- the next one from that sender device? Both need the chain fields the
-- envelope carries, so they are stored rather than discarded.

ALTER TABLE inbox ADD COLUMN device_sequence INTEGER NOT NULL DEFAULT 0;
ALTER TABLE inbox ADD COLUMN chain_id TEXT;
ALTER TABLE inbox ADD COLUMN previous_chain_id TEXT;

-- Digest of the ciphertext this message arrived as.
--
-- Deduplication compares it: the same message ID arriving with different
-- ciphertext is a substitution, not a repeat, and the two must not be
-- confused (PRD section 12.5).
ALTER TABLE inbox ADD COLUMN ciphertext_sha256 TEXT;

-- Local arrival order.
--
-- Display and thread ordering use this rather than `created_at`, which the
-- sender chooses and can therefore backdate to place a message wherever it
-- likes in someone else's view.
ALTER TABLE inbox ADD COLUMN arrival_sequence INTEGER;

-- A sender device may occupy each sequence and chain link once. Partial, so
-- rows written before this migration do not collide on the default zero.
CREATE UNIQUE INDEX inbox_sender_sequence
    ON inbox (channel_id, sender_device, device_sequence)
    WHERE device_sequence > 0;

CREATE UNIQUE INDEX inbox_chain
    ON inbox (channel_id, chain_id)
    WHERE chain_id IS NOT NULL;

CREATE INDEX inbox_arrival ON inbox (channel_id, arrival_sequence);

-- The last message accepted from each sender device.
--
-- This is what makes "the next one" decidable. Kept in its own table so the
-- check and the advance happen in one write under the same lock, which is
-- what stops two concurrent fetches from both accepting sequence N.
CREATE TABLE inbound_chain (
    channel_id    TEXT NOT NULL,
    sender_device TEXT NOT NULL,
    last_sequence INTEGER NOT NULL,
    last_chain_id TEXT NOT NULL,
    PRIMARY KEY (channel_id, sender_device),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE
) STRICT;

-- Messages whose predecessor has not arrived yet.
--
-- A gap is not a reason to drop a message: lazy and partial fetching are
-- supported, so the predecessor may simply be later in the queue. It is also
-- not a reason to accept out of order, since per-device order is a protocol
-- guarantee others rely on. So the message waits, and is reconsidered when
-- its predecessor lands.
CREATE TABLE inbound_hold (
    message_id        TEXT PRIMARY KEY,
    channel_id        TEXT NOT NULL,
    sender_device     TEXT NOT NULL,
    device_sequence   INTEGER NOT NULL,
    chain_id          TEXT NOT NULL,
    previous_chain_id TEXT,
    ciphertext_sha256 TEXT NOT NULL,
    ciphertext        BLOB NOT NULL,
    held_at           TEXT NOT NULL,
    UNIQUE (channel_id, sender_device, device_sequence),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE
) STRICT;

CREATE INDEX inbound_hold_waiting
    ON inbound_hold (channel_id, sender_device, device_sequence);
