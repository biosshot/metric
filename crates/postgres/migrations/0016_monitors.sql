CREATE TABLE metric.monitor_project_locks (
    project_id integer PRIMARY KEY REFERENCES metric.projects(id)
);
CREATE TABLE metric.monitors (
    monitor_id bytea PRIMARY KEY CHECK(octet_length(monitor_id)=16),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    kind smallint NOT NULL CHECK(kind IN(0,1)),
    slug text NOT NULL CHECK(octet_length(slug) BETWEEN 1 AND 64),
    name text NOT NULL CHECK(octet_length(name) BETWEEN 1 AND 128),
    environment_id bytea NOT NULL CHECK(octet_length(environment_id)=16),
    environment text NOT NULL CHECK(octet_length(environment) BETWEEN 1 AND 64),
    enabled boolean NOT NULL,
    managed_by_web boolean NOT NULL,
    revision bigint NOT NULL CHECK(revision>0),
    config bytea NOT NULL CHECK(octet_length(config) BETWEEN 2 AND 65538),
    config_size integer NOT NULL CHECK(config_size BETWEEN 1 AND 65536),
    next_expected_at bigint NOT NULL CHECK(next_expected_at BETWEEN -62135596800000 AND 253402300799999),
    due_at bigint NOT NULL CHECK(due_at BETWEEN -62135596800000 AND 253402300799999),
    lease_until bigint CHECK(lease_until BETWEEN -62135596800000 AND 253402300799999),
    last_run_id bytea CHECK(octet_length(last_run_id)=16),
    last_status smallint CHECK(last_status BETWEEN 0 AND 4),
    last_check_in_at bigint CHECK(last_check_in_at BETWEEN -62135596800000 AND 253402300799999),
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    updated_at bigint NOT NULL CHECK(updated_at BETWEEN -62135596800000 AND 253402300799999 AND updated_at>=created_at),
    UNIQUE(project_id,monitor_id)
);
CREATE INDEX monitor_project_page ON metric.monitors(project_id,updated_at DESC,monitor_id DESC);
CREATE INDEX monitor_cron_due ON metric.monitors(due_at,monitor_id) WHERE kind=0 AND enabled;
CREATE INDEX monitor_uptime_due ON metric.monitors(due_at,monitor_id) WHERE kind=1 AND enabled;
CREATE TABLE metric.monitor_runs (
    run_id bytea PRIMARY KEY CHECK(octet_length(run_id)=16),
    project_id integer NOT NULL,
    monitor_id bytea NOT NULL,
    check_in_id bytea CHECK(octet_length(check_in_id)=16),
    status smallint NOT NULL CHECK(status BETWEEN 0 AND 4),
    source smallint NOT NULL CHECK(source IN(0,1)),
    scheduled_for bigint CHECK(scheduled_for BETWEEN -62135596800000 AND 253402300799999),
    started_at bigint NOT NULL CHECK(started_at BETWEEN -62135596800000 AND 253402300799999),
    finished_at bigint CHECK(finished_at BETWEEN -62135596800000 AND 253402300799999 AND finished_at>=started_at),
    duration_ms bigint CHECK(duration_ms>=0),
    received_at bigint NOT NULL CHECK(received_at BETWEEN -62135596800000 AND 253402300799999),
    release_id bytea CHECK(octet_length(release_id)=16),
    timeout_at bigint CHECK(timeout_at BETWEEN -62135596800000 AND 253402300799999),
    delete_at bigint CHECK(delete_at BETWEEN -62135596800000 AND 253402300799999),
    http_status integer CHECK(http_status BETWEEN 100 AND 599),
    uptime_failure smallint CHECK(uptime_failure BETWEEN 0 AND 7),
    evaluated_at bigint CHECK(evaluated_at BETWEEN -62135596800000 AND 253402300799999),
    FOREIGN KEY(project_id,monitor_id) REFERENCES metric.monitors(project_id,monitor_id) ON DELETE CASCADE,
    CHECK((status=0)=(finished_at IS NULL)),
    CHECK((source=0)=(check_in_id IS NOT NULL)),
    CHECK((http_status IS NULL AND uptime_failure IS NULL) OR (source=1 AND status<>0))
);
CREATE INDEX monitor_run_history ON metric.monitor_runs(project_id,monitor_id,started_at DESC,run_id DESC);
CREATE INDEX monitor_run_timeout ON metric.monitor_runs(timeout_at,run_id) WHERE status=0 AND timeout_at IS NOT NULL;
CREATE INDEX monitor_run_expiry ON metric.monitor_runs(delete_at,project_id,run_id) WHERE delete_at IS NOT NULL;
CREATE INDEX monitor_run_alerts ON metric.monitor_runs(received_at,run_id)
    WHERE evaluated_at IS NULL AND (status IN(2,3,4) OR (status=1 AND http_status IS NOT NULL));
