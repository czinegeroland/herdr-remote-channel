-- What a sender has to remember about its own message to judge a receipt
-- (PRD sections 18.2 and 18.3).
--
-- `accept_receipt` refuses a report from a device that was never addressed,
-- and it can only do that against what this installation actually sent. The
-- outbox recorded how to publish a message — sequence, chain, ciphertext —
-- but nothing about who it was for, so a receipt could not be checked at all
-- and receipts were never wired up.
--
-- These facts are deliberately not read back out of the published object.
-- The published envelope is the sender's own claim, and re-reading it to
-- check a receipt would verify the receipt against whatever the transport
-- currently holds rather than against what this installation meant to send.
-- A rewritten history would then validate its own receipts.

ALTER TABLE outbox ADD COLUMN thread_id TEXT;
ALTER TABLE outbox ADD COLUMN kind TEXT;

-- One row per intended recipient device. A separate table rather than a
-- packed list, because "was this device a recipient" is the question asked,
-- and answering it with a LIKE over a JSON array would match a device whose
-- identifier is a prefix of another's.
CREATE TABLE outbox_recipient (
    message_id TEXT NOT NULL,
    device_id  TEXT NOT NULL,
    PRIMARY KEY (message_id, device_id),
    FOREIGN KEY (message_id) REFERENCES outbox (message_id) ON DELETE CASCADE
) STRICT;
