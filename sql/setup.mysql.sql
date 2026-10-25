-- The audit table.
--
-- One row per change to an audited table. `before_content` and `after_content`
-- hold the audited row as JSON; `updated_by_id` holds the user who made the
-- change and is NULL for changes nobody is behind (webhooks, background jobs).
-- The request columns are NULL for the same non-HTTP writes.
--
-- The table is append-only: nothing updates or deletes an audit row.
--
-- The indexes are declared inline because MySQL has no `CREATE INDEX IF NOT
-- EXISTS`, and this script must be safe to run more than once.

CREATE TABLE IF NOT EXISTS audits (
    id              BINARY(16)   PRIMARY KEY,
    created_at      TIMESTAMP    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    auditable_table VARCHAR(255) NOT NULL,
    auditable_id    VARCHAR(255) NOT NULL,
    action          VARCHAR(10)  NOT NULL,
    before_content  JSON,
    after_content   JSON,
    updated_by_id   VARCHAR(255),
    -- Request details. NULL when the change did not come from a request.
    ip_address      TEXT,
    url             TEXT,
    user_agent      TEXT,

    -- Looking up the history of one row: "what happened to this user?".
    INDEX audits_auditable_idx (auditable_table, auditable_id),
    -- Looking up the history of one user: "what did this user change?".
    INDEX audits_updated_by_id_idx (updated_by_id),
    -- Reading the trail in order.
    INDEX audits_created_at_idx (created_at),

    CONSTRAINT audits_action_check CHECK (action IN ('create', 'update', 'delete'))
);
