-- The Herdr plugin inbox (PRD section 23.2) must show an attachment count
-- and total size, and must distinguish a prompt request from an ordinary
-- message. All three are derived from the signed envelope at acceptance and
-- recorded here so a short-lived plugin process can render a row without
-- decrypting anything.
--
-- Rows accepted before this migration report zero. That is a gap in the
-- record rather than a claim about those messages, and it is the same
-- treatment migration 006 gave outbox rows without reseal material: an older
-- row is honest about not knowing rather than guessing.
ALTER TABLE inbox ADD COLUMN attachment_count INTEGER NOT NULL DEFAULT 0;
ALTER TABLE inbox ADD COLUMN attachment_bytes INTEGER NOT NULL DEFAULT 0;
ALTER TABLE inbox ADD COLUMN prompt_request INTEGER NOT NULL DEFAULT 0;
