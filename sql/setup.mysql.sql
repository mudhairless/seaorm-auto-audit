-- The audit table.
--
-- One row per change to an audited table. `before_content` and `after_content`
-- hold the audited row as JSON; `updated_by_id` holds the user who made the
-- change and is NULL for changes nobody is behind (webhooks, background jobs).
-- The request columns are NULL for the same non-HTTP writes.
--
-- The table is append-only: nothing updates or deletes an audit row.

CREATE TABLE IF NOT EXISTS audits (
    id              CHAR(36)     PRIMARY KEY,
    created_at      TIMESTAMP    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    auditable_table VARCHAR(255) NOT NULL,
    auditable_id    CHAR(36)     NOT NULL,
    action          VARCHAR(10)  NOT NULL,
    before_content  JSON,
    after_content   JSON,
    updated_by_id   CHAR(36),
    -- Request details. NULL when the change did not come from a request.
    ip_address      TEXT,
    url             TEXT,
    user_agent      TEXT,

    CONSTRAINT audits_action_check CHECK (action IN ('create', 'update', 'delete'))
);

-- Looking up the history of one row: "what happened to this user?".
CREATE INDEX audits_auditable_idx
    ON audits (auditable_table, auditable_id);

-- Looking up the history of one user: "what did this user change?".
CREATE INDEX audits_updated_by_id_idx
    ON audits (updated_by_id);

-- Reading the trail in order.
CREATE INDEX audits_created_at_idx
    ON audits (created_at);
