-- Exact domain strings are UTF8 bytea, including valid U+0000. The separate
-- search projection never replaces a user's original title or identity.
CREATE TABLE metric.issues (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    issue_id bytea NOT NULL CHECK (octet_length(issue_id)=16),
    grouping_key bytea NOT NULL CHECK (octet_length(grouping_key)=34 AND substring(grouping_key FROM 1 FOR 2)<>decode('0000','hex')),
    title bytea NOT NULL CHECK (octet_length(title) BETWEEN 1 AND 512),
    culprit bytea CHECK (octet_length(culprit) BETWEEN 1 AND 256),
    search_projection text NOT NULL CHECK (octet_length(search_projection)<=2048),
    title_search tsvector GENERATED ALWAYS AS (pg_catalog.to_tsvector('pg_catalog.simple'::regconfig,search_projection)) STORED,
    first_seen bigint NOT NULL CHECK (first_seen BETWEEN -62135596800000 AND 253402300799999),
    last_seen bigint NOT NULL CHECK (last_seen BETWEEN -62135596800000 AND 253402300799999 AND last_seen>=first_seen),
    first_event_id bytea NOT NULL CHECK (octet_length(first_event_id)=16),
    latest_event_id bytea NOT NULL CHECK (octet_length(latest_event_id)=16),
    representative_event_id bytea NOT NULL CHECK (octet_length(representative_event_id)=16),
    occurrence_count bigint NOT NULL CHECK (occurrence_count>0),
    status smallint NOT NULL DEFAULT 0 CHECK (status BETWEEN 0 AND 2),
    assignee bytea CHECK (octet_length(assignee)=17 AND get_byte(assignee,0) BETWEEN 1 AND 3),
    workflow_at bigint CHECK (workflow_at BETWEEN -62135596800000 AND 253402300799999),
    workflow_actor bytea CHECK (octet_length(workflow_actor)=17 AND get_byte(workflow_actor,0) BETWEEN 1 AND 3),
    regression_at bigint CHECK (regression_at BETWEEN -62135596800000 AND 253402300799999),
    regression_event_id bytea CHECK (octet_length(regression_event_id)=16),
    regression_count bigint CHECK (regression_count>0),
    regression_release bytea CHECK (octet_length(regression_release) BETWEEN 1 AND 200),
    first_release bytea CHECK (octet_length(first_release) BETWEEN 1 AND 200),
    last_release bytea CHECK (octet_length(last_release) BETWEEN 1 AND 200),
    grouping_body bytea NOT NULL CHECK (octet_length(grouping_body) BETWEEN 2 AND 131072),
    PRIMARY KEY (project_id,issue_id),
    CHECK ((status=0 AND workflow_at IS NULL AND workflow_actor IS NULL) OR (status<>0 AND workflow_at IS NOT NULL AND workflow_actor IS NOT NULL)),
    CHECK ((regression_at IS NULL AND regression_event_id IS NULL AND regression_count IS NULL AND regression_release IS NULL) OR (regression_at IS NOT NULL AND regression_event_id IS NOT NULL AND regression_count IS NOT NULL))
);
CREATE INDEX issue_project_timeline ON metric.issues(project_id,last_seen DESC,issue_id DESC);
CREATE INDEX issue_status_timeline ON metric.issues(project_id,status,last_seen DESC,issue_id DESC);
CREATE INDEX issue_release_new_timeline ON metric.issues(project_id,first_release,last_seen DESC,issue_id DESC) WHERE first_release IS NOT NULL;
CREATE INDEX issue_release_regression_timeline ON metric.issues(project_id,regression_release,last_seen DESC,issue_id DESC) WHERE regression_release IS NOT NULL;
CREATE INDEX issue_title_text ON metric.issues USING gin(title_search);
CREATE TABLE metric.issue_transitions (
    project_id integer NOT NULL,
    transition_id bytea NOT NULL CHECK (octet_length(transition_id)=16),
    issue_id bytea NOT NULL CHECK (octet_length(issue_id)=16),
    kind smallint NOT NULL CHECK (kind BETWEEN 1 AND 3),
    event_id bytea NOT NULL CHECK (octet_length(event_id)=16),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    PRIMARY KEY (project_id,transition_id),
    FOREIGN KEY (project_id,issue_id) REFERENCES metric.issues(project_id,issue_id)
);
CREATE INDEX issue_notification_ready ON metric.issue_transitions(created_at,project_id,transition_id);
CREATE TABLE metric.issue_activities (
    project_id integer NOT NULL,
    activity_id bytea NOT NULL CHECK (octet_length(activity_id)=16),
    issue_id bytea NOT NULL CHECK (octet_length(issue_id)=16),
    kind smallint NOT NULL CHECK (kind BETWEEN 1 AND 6),
    actor bytea NOT NULL CHECK (octet_length(actor)=17 AND get_byte(actor,0) BETWEEN 1 AND 3),
    assignee bytea CHECK (octet_length(assignee)=17 AND get_byte(assignee,0) BETWEEN 1 AND 3),
    event_id bytea CHECK (octet_length(event_id)=16),
    at bigint NOT NULL CHECK (at BETWEEN -62135596800000 AND 253402300799999),
    PRIMARY KEY (project_id,activity_id),
    FOREIGN KEY (project_id,issue_id) REFERENCES metric.issues(project_id,issue_id)
);
CREATE INDEX issue_activity_timeline ON metric.issue_activities(project_id,issue_id,at DESC,activity_id DESC);
