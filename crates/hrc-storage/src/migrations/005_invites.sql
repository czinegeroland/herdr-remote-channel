-- Invites this installation issued (PRD section 15.1).
--
-- Local bookkeeping only. The control log is the authority on whether an
-- invite is open, spent, or revoked — every participant derives that from the
-- published chain — and this table exists so an administrator can see who
-- they invited and when, which the chain deliberately does not record.
--
-- The secret is not here. It is handed to the invited person and never
-- stored, because an invite secret at rest is a second copy of something that
-- only ever needed to exist once.

CREATE TABLE invite (
    invite_id    TEXT NOT NULL,
    channel_id   TEXT NOT NULL,
    -- Who the administrator meant it for, in their own words. Local text.
    intended_for TEXT NOT NULL,
    expires_at   TEXT NOT NULL,
    created_at   TEXT NOT NULL,
    state        TEXT NOT NULL,
    PRIMARY KEY (channel_id, invite_id),
    FOREIGN KEY (channel_id) REFERENCES channel (channel_id) ON DELETE CASCADE,
    CHECK (state IN ('open', 'consumed', 'revoked'))
) STRICT;

CREATE INDEX invite_open ON invite (channel_id, state, expires_at);
