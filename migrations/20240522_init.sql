-- GP2F event log schema
-- Apply with: psql $DATABASE_URL -f migrations/20240522_init.sql

CREATE TABLE IF NOT EXISTS event_log (
    tenant_id    TEXT        NOT NULL,
    workflow_id  TEXT        NOT NULL,
    instance_id  TEXT        NOT NULL,
    seq          BIGINT      NOT NULL,
    op_id        TEXT        NOT NULL,
    ingested_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    hlc_ts       BIGINT      NOT NULL,
    outcome      TEXT        NOT NULL, -- 'ACCEPTED' or 'REJECTED'
    payload      JSONB       NOT NULL, -- full ClientMessage JSON for replay
    PRIMARY KEY (tenant_id, workflow_id, instance_id, seq)
);

-- Index for efficient replay ordered by sequence number
CREATE INDEX IF NOT EXISTS idx_event_log_replay
    ON event_log (tenant_id, workflow_id, instance_id, seq ASC);
