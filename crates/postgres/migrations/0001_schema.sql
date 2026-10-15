-- Immutable adapter infrastructure. Capability tables follow in later migrations.
CREATE TABLE metric.schema_state (
    singleton boolean PRIMARY KEY DEFAULT true CHECK (singleton),
    format_id text NOT NULL CHECK (format_id = 'metric-postgres-v1'),
    generation bigint NOT NULL CHECK (generation >= 0)
);
INSERT INTO metric.schema_state (format_id, generation) VALUES ('metric-postgres-v1', 0);

-- Data writes and each checkpoint are committed in the same transaction.
CREATE TABLE metric.data_migrations (
    step_id text PRIMARY KEY,
    generation bigint NOT NULL CHECK (generation > 0),
    checksum bytea NOT NULL,
    checkpoint text CHECK (octet_length(checkpoint) <= 4096),
    complete boolean NOT NULL DEFAULT false
);
