ALTER TABLE metric.projects
    ADD COLUMN debug_file_revision bigint NOT NULL DEFAULT 0 CHECK (debug_file_revision>=0),
    ADD COLUMN artifact_revision bigint NOT NULL DEFAULT 0 CHECK (artifact_revision>=0);
CREATE TABLE metric.error_events (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    event_id bytea NOT NULL CHECK (octet_length(event_id)=16),
    event_key bytea GENERATED ALWAYS AS (pg_catalog.int4send(project_id)||event_id) STORED,
    received_at bigint NOT NULL CHECK (received_at BETWEEN -62135596800000 AND 253402300799999),
    policy_revision bigint NOT NULL CHECK (policy_revision>0),
    body bytea NOT NULL CHECK (octet_length(body)>=2),
    processing_state smallint NOT NULL DEFAULT 0 CHECK (processing_state BETWEEN 0 AND 2),
    attempts integer NOT NULL DEFAULT 0 CHECK (attempts>=0),
    next_attempt_at bigint CHECK (next_attempt_at BETWEEN -62135596800000 AND 253402300799999),
    last_error_code integer CHECK (last_error_code IN (1,2,3,4,5,6,7,8,10,11,12,13,20,30,31,40,41,50,51,52,60)),
    PRIMARY KEY (project_id,event_id),
    UNIQUE (event_key),
    CHECK ((processing_state=0 AND next_attempt_at IS NOT NULL) OR (processing_state<>0 AND next_attempt_at IS NULL))
);
CREATE INDEX event_pending_due ON metric.error_events(next_attempt_at,received_at,event_key) WHERE processing_state=0;
CREATE INDEX event_project_received ON metric.error_events(project_id,received_at,event_id);
-- One scoped reference registry for Error/Feedback payloads. The owning stores
-- write it in the same transaction and lifecycle/archive cleanup owns its removal.
CREATE TABLE metric.blob_references (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    event_id bytea NOT NULL CHECK (octet_length(event_id)=16),
    object_id bytea NOT NULL CHECK (octet_length(object_id)=16),
    source_kind smallint NOT NULL CHECK (source_kind IN (0,1)),
    blob_key text NOT NULL CHECK (octet_length(blob_key) BETWEEN 1 AND 512),
    PRIMARY KEY (project_id,event_id,object_id,source_kind)
);
