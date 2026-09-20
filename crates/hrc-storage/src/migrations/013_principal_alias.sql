-- Locally chosen display names for verified principals (PRD section 23.2).
--
-- Until this table existed the inbox printed a truncated principal ID in the
-- sender column -- the one column on every row a person actually reads --
-- because there was nowhere to put a name. Section 19.1 forbids showing a
-- name the *sender* chose, and that is still true. A name the *receiver*
-- chose is a different thing: it is locally resolved, exactly like the
-- channel name beside it.
--
-- Keyed by channel as well as principal. The same principal can be a member
-- of two channels, and a person may reasonably call them different things in
-- each; more importantly, a name assigned while verifying someone in one
-- channel is not evidence about who they are in another.
--
-- `assigned_at` is kept because an alias is a trust artefact. It is written
-- at the moment a human completed a safety-phrase comparison, and knowing
-- when that happened is part of knowing what the name is worth.
--
-- There is deliberately no `source` column. Every row here is human-assigned
-- through the trusted interface; a row written any other way would be a
-- name the local human did not choose, which is the thing this table exists
-- to avoid.
CREATE TABLE principal_alias (
    channel_id TEXT NOT NULL,
    principal TEXT NOT NULL,
    display_name TEXT NOT NULL,
    assigned_at TEXT NOT NULL,
    PRIMARY KEY (channel_id, principal)
) STRICT;
