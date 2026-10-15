CREATE TABLE metric.session_project_locks (
    project_id integer PRIMARY KEY REFERENCES metric.projects(id)
);
CREATE TABLE metric.sessions (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    session_id bytea NOT NULL CHECK (octet_length(session_id)=16),
    release_id bytea NOT NULL CHECK (octet_length(release_id)=16),
    environment_id bytea NOT NULL CHECK (octet_length(environment_id)=16),
    started_at bigint NOT NULL CHECK (started_at BETWEEN -62135596800000 AND 253402300799999),
    last_update bigint NOT NULL CHECK (last_update BETWEEN -62135596800000 AND 253402300799999 AND last_update>=started_at),
    state smallint NOT NULL CHECK (state BETWEEN 0 AND 3),
    sequence bytea CHECK (octet_length(sequence)=8),
    finished_at bigint CHECK (finished_at BETWEEN -62135596800000 AND 253402300799999 AND finished_at>=started_at AND finished_at<=last_update),
    duration_ms bigint CHECK (duration_ms>=0),
    user_digest bytea CHECK (octet_length(user_digest)=16),
    expire_at bigint CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999),
    archive_due bigint CHECK (archive_due BETWEEN -62135596800000 AND 253402300799999),
    archive_segment bytea CHECK (octet_length(archive_segment)=16),
    PRIMARY KEY (project_id,session_id),
    CHECK ((state=0 AND finished_at IS NULL AND duration_ms IS NULL AND expire_at IS NULL AND archive_due IS NULL) OR (state<>0 AND finished_at IS NOT NULL AND ((expire_at IS NULL)<>(archive_due IS NULL))))
);
CREATE INDEX session_project_started ON metric.sessions(project_id,started_at,session_id);
CREATE INDEX session_stale_active ON metric.sessions(last_update,project_id,session_id) WHERE state=0;
CREATE INDEX session_expiry_due ON metric.sessions(expire_at,project_id,session_id) WHERE expire_at IS NOT NULL;
CREATE INDEX session_archive_due ON metric.sessions(archive_due,project_id,session_id) WHERE archive_due IS NOT NULL;
CREATE TABLE metric.session_stats_hourly (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    release_id bytea NOT NULL CHECK (octet_length(release_id)=16),
    environment_id bytea NOT NULL CHECK (octet_length(environment_id)=16),
    hour bigint NOT NULL CHECK (hour BETWEEN -62135596800000 AND 253402300799999 AND hour%3600000=0),
    sessions bigint NOT NULL DEFAULT 0 CHECK (sessions>=0),
    crashed bigint NOT NULL DEFAULT 0 CHECK (crashed>=0),
    abnormal bigint NOT NULL DEFAULT 0 CHECK (abnormal>=0),
    exited bigint NOT NULL DEFAULT 0 CHECK (exited>=0),
    user_sketch bit(1024) NOT NULL DEFAULT B'0'::bit(1024),
    crashed_user_sketch bit(1024) NOT NULL DEFAULT B'0'::bit(1024),
    expire_at bigint NOT NULL CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999),
    PRIMARY KEY (project_id,release_id,environment_id,hour),
    CHECK (crashed+abnormal+exited<=sessions)
);
CREATE INDEX session_health_release_timeline ON metric.session_stats_hourly(project_id,release_id,hour,environment_id) WHERE sessions>0;
CREATE INDEX session_stats_expiry_due ON metric.session_stats_hourly(expire_at,project_id,release_id,environment_id,hour);
