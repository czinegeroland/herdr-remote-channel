-- Which notifications have already been raised (PRD section 23.3).
--
-- Without this, "has this been notified?" is answered from the memory of a
-- process that lives for as long as one pane render. Every refresh of the
-- inbox pane therefore re-raised every notification, and restarting Herdr
-- re-raised all of them again. A person who left a message pending on
-- purpose was told about it once per second.
--
-- Keyed by message and kind rather than by message alone, because one
-- message can legitimately raise two different notifications over its life:
-- an arrival and, later, a tamper halt on the channel carrying it. The kind
-- is the `Notification` variant's serialized tag, which is a closed set in
-- `hrc-herdr` and never sender-chosen text.
--
-- `resolved_at` rather than a delete, so the record of having notified
-- survives the message being decided. A deleted row and a row that was never
-- written are indistinguishable, and the difference is exactly what stops a
-- decided message from being announced again by a pane that reads a stale
-- snapshot.
CREATE TABLE notification (
    message_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    channel_id TEXT NOT NULL,
    notified_at TEXT NOT NULL,
    resolved_at TEXT,
    PRIMARY KEY (message_id, kind)
) STRICT;

CREATE INDEX notification_unresolved
    ON notification (channel_id, resolved_at);
