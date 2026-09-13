-- Delivery and read receipts (PRD sections 18.2 and 18.3).
--
-- Receipt state is per reporting device rather than per message. A message
-- addressed to a principal reaches every active device that principal has,
-- and "delivered" from one of them is not the same claim as "delivered"
-- everywhere. Collapsing them would let one device's report stand in for a
-- fleet the sender never heard from.

CREATE TABLE delivery_receipt (
    message_id         TEXT NOT NULL,
    channel_id         TEXT NOT NULL,
    -- Verified reporter, resolved from the roster rather than from the body.
    reporter_principal TEXT NOT NULL,
    reporter_device    TEXT NOT NULL,
    state              TEXT NOT NULL,
    rejection_code     TEXT,
    -- The receiver's clock, which is theirs and not ours.
    reported_at        TEXT NOT NULL,
    -- Our clock, which is what local ordering can rely on.
    recorded_at        TEXT NOT NULL,
    -- One report per device per state: a device may report delivered and
    -- later read, but repeating a state it already reported changes nothing.
    PRIMARY KEY (message_id, reporter_device, state),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE,
    CHECK (state IN ('delivered', 'read', 'accepted', 'rejected'))
) STRICT;

CREATE INDEX delivery_receipt_message ON delivery_receipt (channel_id, message_id);
