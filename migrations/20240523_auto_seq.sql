-- Convert event_log.seq to a globally auto-incremented BIGSERIAL.
-- This eliminates the check-then-act race condition in PostgresStore::append
-- by letting the database generate a unique sequence number atomically.

-- Create a dedicated sequence for event_log.seq
CREATE SEQUENCE IF NOT EXISTS event_log_seq;

-- Drop the composite primary key that included seq as a per-partition value
ALTER TABLE event_log DROP CONSTRAINT IF EXISTS event_log_pkey;

-- Make seq auto-generated (globally unique across all partitions)
ALTER TABLE event_log ALTER COLUMN seq SET DEFAULT nextval('event_log_seq');
ALTER SEQUENCE event_log_seq OWNED BY event_log.seq;

-- Use seq alone as the primary key (globally unique, no per-partition composite needed)
ALTER TABLE event_log ADD PRIMARY KEY (seq);

-- Per-instance ordering is still correctly provided by the existing index:
--   idx_event_log_replay (tenant_id, workflow_id, instance_id, seq ASC)
