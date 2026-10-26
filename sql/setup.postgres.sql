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
-- application, so a foreign key is optional, and the form it takes depends on
-- the user table's key type.
--
-- For a text user key, the foreign key goes on the column directly:
--
-- ALTER TABLE audits
--     ADD CONSTRAINT audits_updated_by_id_fkey
--     FOREIGN KEY (updated_by_id) REFERENCES users (id);
--
-- A text column cannot reference a uuid user key. For a uuid user key, add a
-- generated uuid column and put the foreign key on that column instead. Use
-- this form only when every actor id is a uuid: the cast fails on a row whose
-- `updated_by_id` holds anything else.
--
-- ALTER TABLE audits
--     ADD COLUMN updated_by_uuid uuid
--         GENERATED ALWAYS AS (updated_by_id::uuid) STORED;
-- ALTER TABLE audits
--     ADD CONSTRAINT audits_updated_by_uuid_fkey
--         FOREIGN KEY (updated_by_uuid) REFERENCES users (id);
