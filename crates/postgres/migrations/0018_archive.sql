CREATE FUNCTION metric.valid_archive_sources(kind smallint,project integer,ids bytea[])
RETURNS boolean LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT array_ndims(ids)=1 AND array_lower(ids,1)=1
        AND cardinality(ids) BETWEEN 1 AND 10000
        AND array_position(ids,NULL) IS NULL
        AND (SELECT count(DISTINCT value)=cardinality(ids)
            AND bool_and(octet_length(value)=CASE WHEN kind=0 THEN 20 ELSE 16 END
                AND (kind<>0 OR substring(value FROM 1 FOR 4)=pg_catalog.int4send(project)))
            FROM unnest(ids) AS source(value))
$$;
CREATE TABLE metric.archive_lanes (
    kind smallint PRIMARY KEY CHECK(kind BETWEEN 0 AND 4)
);
INSERT INTO metric.archive_lanes(kind) VALUES(0),(1),(2),(3),(4);
CREATE TABLE metric.archive_manifests (
    segment_id bytea PRIMARY KEY CHECK(octet_length(segment_id)=16),
    kind smallint NOT NULL CHECK(kind BETWEEN 0 AND 4),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    received_from bigint NOT NULL CHECK(received_from BETWEEN -62135596800000 AND 253402300799999),
    received_to bigint NOT NULL CHECK(received_to BETWEEN received_from AND 253402300799999),
    object_key text NOT NULL UNIQUE CHECK(octet_length(object_key) BETWEEN 1 AND 512),
    schema_version smallint NOT NULL DEFAULT 1 CHECK(schema_version=1),
    source_ids bytea[] NOT NULL CHECK(metric.valid_archive_sources(kind,project_id,source_ids)),
    record_count integer GENERATED ALWAYS AS(cardinality(source_ids)) STORED,
    state smallint NOT NULL DEFAULT 0 CHECK(state IN (0,1)),
    source_committed boolean NOT NULL DEFAULT false,
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    completed_at bigint CHECK(completed_at BETWEEN -62135596800000 AND 253402300799999),
    stored_bytes bigint CHECK(stored_bytes>=0),
    checksum bytea CHECK(octet_length(checksum)=32),
    CHECK((state=0 AND NOT source_committed AND completed_at IS NULL AND stored_bytes IS NULL AND checksum IS NULL)
        OR (state=1 AND completed_at IS NOT NULL AND stored_bytes IS NOT NULL AND checksum IS NOT NULL))
);
CREATE UNIQUE INDEX archive_one_pending_kind ON metric.archive_manifests(kind) WHERE NOT source_committed;
CREATE INDEX archive_resume ON metric.archive_manifests(kind,state,created_at,segment_id) WHERE NOT source_committed;
CREATE INDEX archive_project_received ON metric.archive_manifests(project_id,received_from,segment_id);
CREATE INDEX log_archive_project_day ON metric.logs(project_id,received_at,log_id) WHERE archive_due IS NOT NULL AND archive_segment IS NULL;
CREATE INDEX span_archive_project_day ON metric.spans(project_id,received_at,record_id) WHERE archive_due IS NOT NULL AND archive_segment IS NULL;
CREATE INDEX session_archive_project_day ON metric.sessions(project_id,finished_at,session_id) WHERE archive_due IS NOT NULL AND archive_segment IS NULL;
CREATE INDEX metric_archive_project_day ON metric.metric_buckets(project_id,received_at,bucket_id) WHERE archive_due IS NOT NULL AND archive_segment IS NULL;
