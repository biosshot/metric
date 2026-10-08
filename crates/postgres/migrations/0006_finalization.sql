ALTER TABLE metric.error_events
    ADD COLUMN occurred_at bigint CHECK (occurred_at BETWEEN -62135596800000 AND 253402300799999),
    ADD COLUMN issue_id bytea CHECK (octet_length(issue_id)=16),
    ADD COLUMN level smallint CHECK (level BETWEEN 0 AND 4),
    ADD COLUMN platform smallint CHECK (platform BETWEEN 0 AND 9),
    ADD COLUMN search_tokens bigint[] NOT NULL DEFAULT '{}' CHECK (cardinality(search_tokens)<=16 AND array_position(search_tokens,NULL) IS NULL),
    ADD COLUMN release_id bytea CHECK (octet_length(release_id)=16),
    ADD COLUMN environment_id bytea CHECK (octet_length(environment_id)=16),
    ADD COLUMN trace_id bytea CHECK (octet_length(trace_id)=16 AND trace_id<>decode(repeat('00',16),'hex')),
    ADD COLUMN span_id bytea CHECK (octet_length(span_id)=8 AND span_id<>decode(repeat('00',8),'hex')),
    ADD COLUMN expire_at bigint CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999),
    ADD COLUMN archive_due bigint CHECK (archive_due BETWEEN -62135596800000 AND 253402300799999),
    ADD COLUMN archive_segment bytea CHECK (octet_length(archive_segment)=16),
    ADD CONSTRAINT finalized_event_complete CHECK (processing_state<>2 OR (occurred_at IS NOT NULL AND issue_id IS NOT NULL AND level IS NOT NULL AND platform IS NOT NULL)),
    ADD CONSTRAINT event_trace_pair CHECK ((trace_id IS NULL)=(span_id IS NULL)),
    ADD CONSTRAINT event_issue_scope FOREIGN KEY (project_id,issue_id) REFERENCES metric.issues(project_id,issue_id);
CREATE INDEX event_issue_timeline ON metric.error_events(project_id,issue_id,occurred_at DESC,event_id DESC) WHERE processing_state=2;
CREATE INDEX event_project_timeline ON metric.error_events(project_id,occurred_at DESC,event_id DESC) WHERE processing_state=2;
CREATE INDEX event_search_tokens ON metric.error_events USING gin(search_tokens) WHERE processing_state=2;
CREATE INDEX event_trace_timeline ON metric.error_events(project_id,trace_id,occurred_at,event_id) WHERE trace_id IS NOT NULL AND processing_state=2;
CREATE INDEX event_expiry_due ON metric.error_events(expire_at,project_id,event_id) WHERE expire_at IS NOT NULL;
CREATE INDEX event_archive_due ON metric.error_events(archive_due,project_id,event_id) WHERE archive_due IS NOT NULL;

CREATE TABLE metric.issue_stats_hourly (
    project_id integer NOT NULL,
    issue_id bytea NOT NULL CHECK (octet_length(issue_id)=16),
    bucket_start bigint NOT NULL CHECK (bucket_start BETWEEN -62135596800000 AND 253402300799999 AND bucket_start%3600000=0),
    bucket_id bytea NOT NULL CHECK (octet_length(bucket_id)=16),
    occurrence_count bigint NOT NULL CHECK (occurrence_count>0),
    expire_at bigint NOT NULL CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999),
    PRIMARY KEY (project_id,issue_id,bucket_start),
    FOREIGN KEY (project_id,issue_id) REFERENCES metric.issues(project_id,issue_id)
);
CREATE INDEX hourly_expiry_due ON metric.issue_stats_hourly(expire_at,project_id,issue_id,bucket_start);
CREATE TABLE metric.project_catalog_usage (
    project_id integer PRIMARY KEY REFERENCES metric.projects(id),
    release_day bigint CHECK (release_day BETWEEN -62135596800000 AND 253402300799999 AND release_day%86400000=0),
    release_count bigint NOT NULL DEFAULT 0 CHECK (release_count>=0),
    environment_count bigint NOT NULL DEFAULT 0 CHECK (environment_count>=0)
);
CREATE TABLE metric.releases (
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    release_id bytea NOT NULL CHECK (octet_length(release_id)=16),
    version bytea NOT NULL CHECK (octet_length(version) BETWEEN 1 AND 200),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    activity_at bigint NOT NULL CHECK (activity_at BETWEEN -62135596800000 AND 253402300799999),
    released_at bigint CHECK (released_at BETWEEN -62135596800000 AND 253402300799999),
    first_seen bigint CHECK (first_seen BETWEEN -62135596800000 AND 253402300799999),
    last_seen bigint CHECK (last_seen BETWEEN -62135596800000 AND 253402300799999),
    first_event_key bytea CHECK (octet_length(first_event_key)=20),
    latest_event_key bytea CHECK (octet_length(latest_event_key)=20),
    explicit boolean NOT NULL DEFAULT false,
    url bytea CHECK (octet_length(url) BETWEEN 1 AND 2048),
    reference bytea CHECK (octet_length(reference) BETWEEN 1 AND 200),
    repositories_body bytea CHECK (octet_length(repositories_body) BETWEEN 2 AND 65536),
    PRIMARY KEY (organization_id,release_id),
    CHECK ((first_seen IS NULL AND last_seen IS NULL AND first_event_key IS NULL AND latest_event_key IS NULL) OR (first_seen IS NOT NULL AND last_seen IS NOT NULL AND first_event_key IS NOT NULL AND latest_event_key IS NOT NULL AND first_seen<=last_seen))
);
CREATE INDEX release_organization_timeline ON metric.releases(organization_id,activity_at DESC,release_id DESC);
ALTER TABLE metric.projects ADD CONSTRAINT project_organization_identity UNIQUE (organization_id,id);
CREATE TABLE metric.release_projects (
    organization_id bigint NOT NULL,
    release_id bytea NOT NULL,
    project_id integer NOT NULL,
    PRIMARY KEY (organization_id,release_id,project_id),
    FOREIGN KEY (organization_id,release_id) REFERENCES metric.releases(organization_id,release_id),
    FOREIGN KEY (organization_id,project_id) REFERENCES metric.projects(organization_id,id)
);
CREATE INDEX release_project_lookup ON metric.release_projects(project_id,organization_id,release_id);
CREATE TABLE metric.environments (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    environment_id bytea NOT NULL CHECK (octet_length(environment_id)=16),
    name bytea NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 64),
    first_seen bigint NOT NULL CHECK (first_seen BETWEEN -62135596800000 AND 253402300799999),
    last_seen bigint NOT NULL CHECK (last_seen BETWEEN -62135596800000 AND 253402300799999 AND last_seen>=first_seen),
    hidden boolean NOT NULL DEFAULT false,
    explicit boolean NOT NULL DEFAULT false,
    PRIMARY KEY (project_id,environment_id)
);
CREATE INDEX environment_project_name ON metric.environments(project_id,name,environment_id);
