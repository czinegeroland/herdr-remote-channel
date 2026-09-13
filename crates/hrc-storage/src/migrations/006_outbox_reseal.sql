-- Re-encryption material for unpublished messages.
--
-- This is an age ciphertext addressed only to the local sending device. It
-- contains the logical envelope before recipient derivation, signing, and
-- channel encryption, so a later roster epoch can replace stale ciphertext
-- without changing the message's identity or content.
ALTER TABLE outbox ADD COLUMN reseal_material BLOB;
