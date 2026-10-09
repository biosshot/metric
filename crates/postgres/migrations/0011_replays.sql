CREATE FUNCTION metric.valid_replay_correlations(ids bytea[]) RETURNS boolean
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT cardinality(ids)<=100
        AND (cardinality(ids)=0 OR (array_ndims(ids)=1 AND array_lower(ids,1)=1))
        AND NOT EXISTS(SELECT 1 FROM unnest(ids) AS item WHERE item IS NULL OR octet_length(item)<>16)
$$;
CREATE TABLE metric.replays (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    replay_id bytea NOT NULL CHECK (octet_length(replay_id)=16),
    started_at bigint NOT NULL CHECK (started_at BETWEEN -62135596800000 AND 253402300799999),
    ended_at bigint NOT NULL CHECK (ended_at BETWEEN -62135596800000 AND 253402300799999 AND ended_at>=started_at),
    received_at bigint NOT NULL CHECK (received_at BETWEEN -62135596800000 AND 253402300799999),
    environment bytea CHECK (octet_length(environment) BETWEEN 1 AND 256),
    release bytea CHECK (octet_length(release) BETWEEN 1 AND 256),
    url bytea CHECK (octet_length(url) BETWEEN 1 AND 256),
    error_ids bytea[] NOT NULL DEFAULT '{}' CHECK (metric.valid_replay_correlations(error_ids)),
    trace_ids bytea[] NOT NULL DEFAULT '{}' CHECK (metric.valid_replay_correlations(trace_ids)),
    expire_at bigint CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999),
    archive_due bigint CHECK (archive_due BETWEEN -62135596800000 AND 253402300799999),
    archive_segment bytea CHECK (octet_length(archive_segment)=16),
    PRIMARY KEY(project_id,replay_id),
    CHECK ((expire_at IS NULL)<>(archive_due IS NULL))
);
CREATE INDEX replay_project_received ON metric.replays(project_id,received_at DESC,replay_id DESC);
CREATE INDEX replay_errors ON metric.replays USING gin(error_ids);
CREATE INDEX replay_traces ON metric.replays USING gin(trace_ids);
CREATE INDEX replay_expiry_due ON metric.replays(expire_at,project_id,replay_id) WHERE expire_at IS NOT NULL;
CREATE INDEX replay_archive_due ON metric.replays(archive_due,project_id,replay_id) WHERE archive_due IS NOT NULL;
CREATE TABLE metric.replay_segments (
    project_id integer NOT NULL,
    replay_id bytea NOT NULL,
    segment_id integer NOT NULL CHECK (segment_id BETWEEN 0 AND 99),
    blob_key text NOT NULL CHECK (octet_length(blob_key) BETWEEN 1 AND 512),
    size bigint NOT NULL CHECK (size>=0),
    checksum bytea NOT NULL CHECK (octet_length(checksum)=32),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    decompressed_bytes bigint NOT NULL CHECK (decompressed_bytes>=0),
    event_count bigint NOT NULL CHECK (event_count BETWEEN 0 AND 4294967295),
    PRIMARY KEY(project_id,replay_id,segment_id),
    FOREIGN KEY(project_id,replay_id) REFERENCES metric.replays(project_id,replay_id) ON DELETE CASCADE
);
CREATE UNIQUE INDEX replay_segment_blob_key ON metric.replay_segments(blob_key);
