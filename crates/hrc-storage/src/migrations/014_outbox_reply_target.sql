-- What an outgoing message answers (PRD sections 18.3 and 23.2).
--
-- The inbox has carried `in_reply_to` since migration 001, so this
-- installation has always known which of *its* messages answered one of
-- ours. It has never recorded the other direction: which of our messages
-- answered one of theirs.
--
-- That asymmetry is why nothing could tell a person about a question they
-- had been asked and had not answered -- the single largest failure mode
-- measured for cooperating coding agents, where a question going unanswered
-- breaks the decision loop it was asked inside. The count needs both
-- directions, and only one of them existed.
--
-- Nullable, and null for every row written before this migration. A message
-- sent by an older build is not evidence that it answered nothing; it is
-- evidence that nobody wrote down what it answered. The queries that use
-- this column therefore say what they can see rather than asserting a
-- question went unanswered on the strength of a column that did not exist
-- when the reply was sent.
ALTER TABLE outbox ADD COLUMN in_reply_to TEXT;

-- The question being answered is what is looked up, not the answer, so the
-- index is on the target rather than on the row's own identifier.
CREATE INDEX outbox_reply_target ON outbox (channel_id, in_reply_to);
