CREATE TABLE metric.feedback (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    feedback_id bytea NOT NULL CHECK (octet_length(feedback_id)=16),
    received_at bigint NOT NULL CHECK (received_at BETWEEN -62135596800000 AND 253402300799999),
    status smallint NOT NULL CHECK (status BETWEEN 0 AND 2),
    status_changed_at bigint NOT NULL CHECK (status_changed_at BETWEEN -62135596800000 AND 253402300799999 AND status_changed_at>=received_at),
    message bytea NOT NULL CHECK (octet_length(message) BETWEEN 1 AND 16384),
    name bytea CHECK (octet_length(name) BETWEEN 1 AND 512),
    contact_email bytea CHECK (octet_length(contact_email) BETWEEN 1 AND 512),
    url bytea CHECK (octet_length(url) BETWEEN 1 AND 2048),
    associated_event_id bytea CHECK (octet_length(associated_event_id)=16),
    trace_id bytea CHECK (octet_length(trace_id)=16),
    replay_id bytea CHECK (octet_length(replay_id)=16),
    expire_at bigint NOT NULL CHECK (expire_at BETWEEN -62135596800000 AND 253402300799999 AND expire_at>received_at),
    PRIMARY KEY(project_id,feedback_id)
);
CREATE INDEX feedback_project_timeline ON metric.feedback(project_id,received_at DESC,feedback_id DESC);
CREATE INDEX feedback_project_status_timeline ON metric.feedback(project_id,status,received_at DESC,feedback_id DESC);
CREATE INDEX feedback_project_event_timeline ON metric.feedback(project_id,associated_event_id,received_at DESC,feedback_id DESC) WHERE associated_event_id IS NOT NULL;
CREATE INDEX feedback_project_trace_timeline ON metric.feedback(project_id,trace_id,received_at DESC,feedback_id DESC) WHERE trace_id IS NOT NULL;
CREATE INDEX feedback_project_replay_timeline ON metric.feedback(project_id,replay_id,received_at DESC,feedback_id DESC) WHERE replay_id IS NOT NULL;
CREATE INDEX feedback_expiry_due ON metric.feedback(expire_at,project_id,feedback_id);
CREATE TABLE metric.feedback_attachments (
    project_id integer NOT NULL,
    feedback_id bytea NOT NULL,
    ordinal smallint NOT NULL CHECK (ordinal BETWEEN 0 AND 9),
    attachment_id bytea NOT NULL CHECK (octet_length(attachment_id)=16),
    blob_key text NOT NULL CHECK (octet_length(blob_key) BETWEEN 1 AND 512),
    size bigint NOT NULL CHECK (size>=0),
    checksum bytea NOT NULL CHECK (octet_length(checksum)=32),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    filename bytea NOT NULL CHECK (octet_length(filename) BETWEEN 1 AND 128),
    content_type bytea NOT NULL CHECK (octet_length(content_type) BETWEEN 1 AND 128),
    attachment_type bytea NOT NULL CHECK (octet_length(attachment_type) BETWEEN 1 AND 512),
    PRIMARY KEY(project_id,feedback_id,ordinal),
    FOREIGN KEY(project_id,feedback_id) REFERENCES metric.feedback(project_id,feedback_id) ON DELETE CASCADE
);
CREATE FUNCTION metric.remove_feedback_references() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    DELETE FROM metric.blob_references WHERE project_id=OLD.project_id AND event_id=OLD.feedback_id AND source_kind=1;
    RETURN OLD;
END
$$;
CREATE TRIGGER feedback_removed_references AFTER DELETE ON metric.feedback
FOR EACH ROW EXECUTE FUNCTION metric.remove_feedback_references();
