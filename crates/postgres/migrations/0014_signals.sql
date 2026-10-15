CREATE TABLE metric.signal_project_locks (
    project_id integer PRIMARY KEY REFERENCES metric.projects(id)
);
CREATE FUNCTION metric.signal_trigrams(value bytea) RETURNS integer[]
LANGUAGE sql IMMUTABLE STRICT PARALLEL SAFE AS $$
    SELECT coalesce(array_agg(DISTINCT (get_byte(value,n)*65536+get_byte(value,n+1)*256+get_byte(value,n+2))), '{}')
    FROM generate_series(0,octet_length(value)-3) n
$$;
CREATE TABLE metric.logs (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    log_id bytea NOT NULL CHECK(octet_length(log_id)=16),
    received_at bigint NOT NULL CHECK(received_at BETWEEN -62135596800000 AND 253402300799999),
    occurred_at_ns bigint NOT NULL,
    severity integer NOT NULL CHECK(severity BETWEEN 1 AND 6),
    message bytea NOT NULL CHECK(octet_length(message)<=32768),
    message_folded bytea NOT NULL CHECK(octet_length(message_folded)<=65536),
    message_trigrams integer[] GENERATED ALWAYS AS(metric.signal_trigrams(message_folded)) STORED,
    trace_id bytea CHECK(octet_length(trace_id)=16),
    span_id bytea CHECK(octet_length(span_id)=8),
    environment bytea CHECK(octet_length(environment)<=512),
    release bytea CHECK(octet_length(release)<=1024),
    service bytea CHECK(octet_length(service)<=1024),
    body bytea NOT NULL CHECK(octet_length(body) BETWEEN 2 AND 1048578),
    body_size integer NOT NULL CHECK(body_size BETWEEN 1 AND 1048576),
    record_bytes integer GENERATED ALWAYS AS(body_size+octet_length(message)+coalesce(octet_length(environment),0)+coalesce(octet_length(release),0)+coalesce(octet_length(service),0)+256) STORED,
    expire_at bigint CHECK(expire_at BETWEEN -62135596800000 AND 253402300799999),
    archive_due bigint CHECK(archive_due BETWEEN -62135596800000 AND 253402300799999),
    archive_segment bytea CHECK(octet_length(archive_segment)=16),
    PRIMARY KEY(project_id,log_id),
    CHECK((expire_at IS NULL)<>(archive_due IS NULL))
);
CREATE INDEX log_project_time ON metric.logs(project_id,occurred_at_ns DESC,log_id DESC);
CREATE INDEX log_project_severity_time ON metric.logs(project_id,severity,occurred_at_ns DESC,log_id DESC);
CREATE INDEX log_project_trace_time ON metric.logs(project_id,trace_id,occurred_at_ns,log_id) WHERE trace_id IS NOT NULL;
CREATE INDEX log_message_trigrams ON metric.logs USING gin(message_trigrams);
CREATE INDEX log_expiry_due ON metric.logs(expire_at,project_id,log_id) WHERE expire_at IS NOT NULL;
CREATE INDEX log_archive_due ON metric.logs(archive_due,project_id,log_id) WHERE archive_due IS NOT NULL;
CREATE TABLE metric.spans (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    record_id bytea NOT NULL CHECK(octet_length(record_id)=16),
    admission_order bigint GENERATED ALWAYS AS IDENTITY,
    received_at bigint NOT NULL CHECK(received_at BETWEEN -62135596800000 AND 253402300799999),
    started_at_ns bigint NOT NULL,
    duration_ns bigint NOT NULL CHECK(duration_ns>=0),
    trace_id bytea NOT NULL CHECK(octet_length(trace_id)=16),
    span_id bytea NOT NULL CHECK(octet_length(span_id)=8),
    parent_span_id bytea CHECK(octet_length(parent_span_id)=8),
    is_segment boolean NOT NULL,
    operation_class integer NOT NULL CHECK(operation_class BETWEEN 0 AND 11),
    operation bytea NOT NULL CHECK(octet_length(operation)<=512),
    status bytea NOT NULL CHECK(octet_length(status)<=256),
    name bytea NOT NULL CHECK(octet_length(name)<=4096),
    environment bytea CHECK(octet_length(environment)<=512),
    release bytea CHECK(octet_length(release)<=1024),
    service bytea CHECK(octet_length(service)<=1024),
    insight_flags bigint NOT NULL CHECK(insight_flags BETWEEN 0 AND 4294967295),
    dimensions bytea NOT NULL CHECK(octet_length(dimensions) BETWEEN 1 AND 16384),
    body bytea NOT NULL CHECK(octet_length(body) BETWEEN 2 AND 1048578),
    body_size integer NOT NULL CHECK(body_size BETWEEN 1 AND 1048576),
    record_bytes integer GENERATED ALWAYS AS(body_size+octet_length(operation)+octet_length(status)+octet_length(name)+coalesce(octet_length(environment),0)+coalesce(octet_length(release),0)+coalesce(octet_length(service),0)+256) STORED,
    stats_applied boolean NOT NULL DEFAULT false,
    expire_at bigint CHECK(expire_at BETWEEN -62135596800000 AND 253402300799999),
    archive_due bigint CHECK(archive_due BETWEEN -62135596800000 AND 253402300799999),
    archive_segment bytea CHECK(octet_length(archive_segment)=16),
    PRIMARY KEY(project_id,record_id),
    CHECK((expire_at IS NULL)<>(archive_due IS NULL))
);
CREATE INDEX span_project_time ON metric.spans(project_id,started_at_ns DESC,record_id DESC);
CREATE INDEX span_segment_time ON metric.spans(project_id,started_at_ns DESC,record_id DESC) WHERE is_segment;
CREATE INDEX span_project_trace_time ON metric.spans(project_id,trace_id,started_at_ns,span_id,record_id);
CREATE INDEX span_pending_statistics ON metric.spans(project_id,admission_order,record_id) WHERE is_segment AND NOT stats_applied;
CREATE INDEX span_expiry_due ON metric.spans(expire_at,project_id,record_id) WHERE expire_at IS NOT NULL;
CREATE INDEX span_archive_due ON metric.spans(archive_due,project_id,record_id) WHERE archive_due IS NOT NULL;
CREATE TABLE metric.span_stats_hourly (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    hour bigint NOT NULL CHECK(hour BETWEEN -62135596800000 AND 253402300799999 AND hour%3600000=0),
    dimensions bytea NOT NULL CHECK(octet_length(dimensions) BETWEEN 1 AND 16384),
    dimension_key bytea GENERATED ALWAYS AS(sha256(dimensions)) STORED,
    environment bytea CHECK(octet_length(environment)<=512),
    release bytea CHECK(octet_length(release)<=1024),
    service bytea CHECK(octet_length(service)<=1024),
    representative_trace_id bytea NOT NULL CHECK(octet_length(representative_trace_id)=16),
    occurrence_count bigint NOT NULL CHECK(occurrence_count>0),
    failure_count bigint NOT NULL CHECK(failure_count BETWEEN 0 AND occurrence_count),
    duration_sum bigint NOT NULL CHECK(duration_sum>=0),
    samples bigint[] NOT NULL CHECK(cardinality(samples) BETWEEN 1 AND 2048 AND array_ndims(samples)=1 AND array_position(samples,NULL) IS NULL AND 0<=ALL(samples)),
    expire_at bigint NOT NULL CHECK(expire_at BETWEEN -62135596800000 AND 253402300799999),
    PRIMARY KEY(project_id,hour,dimension_key)
);
CREATE INDEX span_statistics_project_time ON metric.span_stats_hourly(project_id,hour DESC,occurrence_count DESC,dimension_key);
CREATE INDEX span_statistics_expiry_due ON metric.span_stats_hourly(expire_at,project_id,hour,dimension_key);

-- The caller owns project-local Signal coordination. Derived statistics and
-- processing markers commit together. Repair passes all sources in full hours;
-- online rollups select at most 10,000 pending records without transferring them.
CREATE FUNCTION metric.refresh_span_stats(p integer,lower_ns bigint,upper_ns bigint,pending boolean,maximum integer,days integer) RETURNS bigint
LANGUAGE plpgsql AS $$
DECLARE expected_groups bigint; saved_groups bigint; marked_sources bigint;
BEGIN
    WITH picked AS MATERIALIZED (
        SELECT record_id,dimensions,environment,release,service,trace_id,duration_ns,status,
            (floor(started_at_ns::numeric/3600000000000)*3600000)::bigint AS hour,
            CASE WHEN pending THEN admission_order ELSE started_at_ns END AS source_order
        FROM metric.spans WHERE project_id=p AND is_segment
            AND (NOT pending OR NOT stats_applied)
            AND (lower_ns IS NULL OR started_at_ns>=lower_ns)
            AND (upper_ns IS NULL OR started_at_ns<upper_ns)
        ORDER BY CASE WHEN pending THEN admission_order ELSE started_at_ns END,record_id
        LIMIT CASE WHEN pending THEN maximum ELSE NULL END
    ), ranked AS (
        SELECT *,row_number() OVER(PARTITION BY hour,dimensions ORDER BY source_order DESC,record_id DESC) AS sample_rank,
            first_value(trace_id) OVER(PARTITION BY hour,dimensions ORDER BY source_order,record_id) AS representative
        FROM picked
    ), grouped AS MATERIALIZED (
        SELECT hour,dimensions,environment,release,service,representative,count(*) AS count,
            count(*) FILTER(WHERE status NOT IN (decode('','hex'),decode('6f6b','hex'),decode('63616e63656c6c6564','hex'))) AS failures,
            sum(duration_ns)::bigint AS duration,
            array_agg(duration_ns ORDER BY source_order,record_id) FILTER(WHERE sample_rank<=2048) AS samples
        FROM ranked GROUP BY hour,dimensions,environment,release,service,representative
    ), saved AS (
        INSERT INTO metric.span_stats_hourly AS current(project_id,hour,dimensions,environment,release,service,representative_trace_id,occurrence_count,failure_count,duration_sum,samples,expire_at)
        SELECT p,hour,dimensions,environment,release,service,representative,count,failures,duration,samples,hour+days::bigint*86400000 FROM grouped
        ON CONFLICT(project_id,hour,dimension_key) DO UPDATE SET
            occurrence_count=current.occurrence_count+EXCLUDED.occurrence_count,
            failure_count=current.failure_count+EXCLUDED.failure_count,
            duration_sum=current.duration_sum+EXCLUDED.duration_sum,
            samples=(current.samples||EXCLUDED.samples)[greatest(1,cardinality(current.samples||EXCLUDED.samples)-2047):cardinality(current.samples||EXCLUDED.samples)]
        WHERE current.dimensions=EXCLUDED.dimensions
        RETURNING 1
    ), marked AS (
        UPDATE metric.spans SET stats_applied=true WHERE project_id=p AND record_id IN(SELECT record_id FROM picked) RETURNING 1
    ) SELECT (SELECT count(*) FROM grouped),(SELECT count(*) FROM saved),(SELECT count(*) FROM marked)
        INTO expected_groups,saved_groups,marked_sources;
    IF expected_groups<>saved_groups THEN RAISE EXCEPTION 'Span statistic identity conflict' USING ERRCODE='23505'; END IF;
    RETURN marked_sources;
END
$$;
