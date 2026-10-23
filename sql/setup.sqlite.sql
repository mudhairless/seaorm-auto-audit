-- The audit table.
--
-- One row per change to an audited table. `before_content` and `after_content`
-- hold the audited row as JSON; `updated_by_id` holds the user who made the
-- change and is NULL for changes nobody is behind (webhooks, background jobs).
-- The request columns are NULL for the same non-HTTP writes.
--
-- The table is append-only: nothing updates or deletes an audit row.

CREATE TABLE IF NOT EXISTS audits (
    id              TEXT        PRIMARY KEY,
    created_at      TEXT        NOT NULL DEFAULT (datetime('now')),
    auditable_table TEXT        NOT NULL,
    auditable_id    TEXT        NOT NULL,
    action          TEXT        NOT NULL CHECK (action IN ('create', 'update', 'delete')),
    before_content  TEXT,
    after_content   TEXT,
    updated_by_id   TEXT,
    -- Request details. NULL when the change did not come from a request.
    ip_address      TEXT,
    url             TEXT,
    user_agent      TEXT
);

-- Looking up the history of one row: "what happened to this user?".
CREATE INDEX IF NOT EXISTS audits_auditable_idx
    ON audits (auditable_table, auditable_id);

-- Looking up the history of one user: "what did this user change?".
CREATE INDEX IF NOT EXISTS audits_updated_by_id_idx
    ON audits (updated_by_id);

-- Reading the trail in order.
CREATE INDEX IF NOT EXISTS audits_created_at_idx
    ON audits (created_at);
