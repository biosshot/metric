CREATE TABLE metric.metric_project_locks (
    project_id integer PRIMARY KEY REFERENCES metric.projects(id)
);
CREATE FUNCTION metric.add_metric_bins(left_counts bigint[],right_counts bigint[]) RETURNS bigint[]
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT array_agg(l+r ORDER BY ordinal)
    FROM unnest(left_counts,right_counts) WITH ORDINALITY AS input(l,r,ordinal)
$$;
CREATE TABLE metric.metric_buckets (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    bucket_id bytea NOT NULL CHECK (octet_length(bucket_id)=16),
    series_id bytea NOT NULL CHECK (octet_length(series_id)=16),
    name bytea NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 524288),
    name_key bytea GENERATED ALWAYS AS (sha256(name)) STORED,
    kind smallint NOT NULL CHECK (kind BETWEEN 1 AND 3),
    unit bytea NOT NULL CHECK (octet_length(unit)<=524288),
    tags bytea NOT NULL CHECK (octet_length(tags) BETWEEN 2 AND 524288),
    bucket_start bigint NOT NULL CHECK (bucket_start BETWEEN -62135596800000 AND 253402300799999),
    bucket_width_seconds bigint NOT NULL CHECK (bucket_width_seconds BETWEEN 1 AND 4294967295),
    received_at bigint NOT NULL CHECK (received_at BETWEEN -62135596800000 AND 253402300799999),
    trace_id bytea CHECK (octet_length(trace_id)=16),
    aggregate_sum double precision NOT NULL CHECK (aggregate_sum NOT IN ('NaN'::float8,'Infinity'::float8,'-Infinity'::float8)),
    measurement_count bigint NOT NULL CHECK (measurement_count>=0),
    last_value double precision CHECK (last_value NOT IN ('NaN'::float8,'Infinity'::float8,'-Infinity'::float8)),
    minimum double precision CHECK (minimum NOT IN ('NaN'::float8,'Infinity'::float8,'-Infinity'::float8)),
    maximum double precision CHECK (maximum NOT IN ('NaN'::float8,'Infinity'::float8,'-Infinity'::float8)),
    bins bigint[] NOT NULL CHECK (cardinality(bins)=64 AND array_ndims(bins)=1 AND array_lower(bins,1)=1 AND array_position(bins,NULL) IS NULL AND 0<=ALL(bins)),
    expire_at bigint CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999),
    archive_due bigint CHECK (archive_due BETWEEN -62135596800000 AND 253402300799999),
    archive_segment bytea CHECK (octet_length(archive_segment)=16),
    PRIMARY KEY(project_id,bucket_id),
    UNIQUE(project_id,series_id,bucket_start,bucket_width_seconds),
    CHECK (octet_length(name)+octet_length(unit)+octet_length(tags)<=524288),
    CHECK ((kind=1 AND last_value IS NULL AND minimum IS NULL AND maximum IS NULL) OR (kind=2 AND last_value IS NOT NULL AND minimum IS NOT NULL AND maximum IS NOT NULL AND minimum<=last_value AND last_value<=maximum) OR (kind=3 AND last_value IS NULL AND minimum IS NOT NULL AND maximum IS NOT NULL AND minimum<=maximum)),
    CHECK ((expire_at IS NULL)<>(archive_due IS NULL))
);
CREATE INDEX metric_project_time_name ON metric.metric_buckets(project_id,bucket_start DESC,bucket_id);
CREATE INDEX metric_project_name_time ON metric.metric_buckets(project_id,name_key,bucket_start DESC,bucket_id);
CREATE INDEX metric_project_series ON metric.metric_buckets(project_id,series_id);
CREATE INDEX metric_expiry_due ON metric.metric_buckets(expire_at,project_id,bucket_id) WHERE expire_at IS NOT NULL;
CREATE INDEX metric_archive_due ON metric.metric_buckets(archive_due,project_id,bucket_id) WHERE archive_due IS NOT NULL;
