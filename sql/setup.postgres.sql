-- The audit table.
--
-- One row per change to an audited table. `before_content` and `after_content`
-- hold the audited row as JSON; `updated_by_id` holds the user who made the
-- change and is NULL for changes nobody is behind (webhooks, background jobs).
-- The request columns are NULL for the same non-HTTP writes.
--
-- The table is append-only: nothing updates or deletes an audit row.

CREATE TABLE IF NOT EXISTS audits (
    id              uuid        PRIMARY KEY,
    created_at      timestamptz NOT NULL DEFAULT now(),
    auditable_table text        NOT NULL,
    auditable_id    text        NOT NULL,
    action          text        NOT NULL,
    before_content  jsonb,
    after_content   jsonb,
    updated_by_id   text,
    -- Request details. NULL when the change did not come from a request.
    ip_address      text,
    url             text,
    user_agent      text,

    CONSTRAINT audits_action_check CHECK (action IN ('create', 'update', 'delete'))
);

-- Looking up the history of one row: "what happened to this user?".
CREATE INDEX IF NOT EXISTS audits_auditable_idx
    ON audits (auditable_table, auditable_id);

-- Looking up the history of one user: "what did this user change?".
CREATE INDEX IF NOT EXISTS audits_updated_by_id_idx
    ON audits (updated_by_id)
    WHERE updated_by_id IS NOT NULL;

-- Reading the trail in order.
CREATE INDEX IF NOT EXISTS audits_created_at_idx
    ON audits (created_at);

-- `auditable_id` and `updated_by_id` are text so one audit table can reference
-- uuid, integer and string keys alike. The user table lives in the host
-- application, so a foreign key is optional. Add one only when `updated_by_id`
-- and the user key have compatible types (for a `uuid` user key, compare the
-- text form with a generated column or a cast).
--
-- ALTER TABLE audits
--     ADD CONSTRAINT audits_updated_by_id_fkey
--     FOREIGN KEY (updated_by_id) REFERENCES users (id);
