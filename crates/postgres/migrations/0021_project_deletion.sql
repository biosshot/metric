-- Durable per-project deletion, with append-only adapter-local dataset codes.
ALTER TABLE metric.projects
    ALTER COLUMN slug DROP NOT NULL,
    ADD COLUMN deleted_at bigint CHECK(deleted_at BETWEEN -62135596800000 AND 253402300799999),
    ADD COLUMN deletion_operation bytea CHECK(octet_length(deletion_operation)=16),
    ADD COLUMN slug_reserved_until bigint CHECK(slug_reserved_until BETWEEN -62135596800000 AND 253402300799999),
    ADD CONSTRAINT live_project_slug CHECK(state=4 OR slug IS NOT NULL);
CREATE INDEX deletion_slug_due ON metric.projects(slug_reserved_until,id) WHERE state=4 AND slug IS NOT NULL;
CREATE TABLE metric.project_deletions (
    operation_id bytea PRIMARY KEY CHECK(octet_length(operation_id)=16),
    plan_version smallint NOT NULL DEFAULT 1 CHECK(plan_version=1),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    organization_id bigint NOT NULL,
    requested_by bigint NOT NULL CHECK(requested_by>0),
    phase smallint NOT NULL DEFAULT 0 CHECK(phase BETWEEN 0 AND 3),
    previous_state smallint NOT NULL CHECK(previous_state IN(0,1)),
    dataset_code integer NOT NULL DEFAULT 10 CHECK(dataset_code BETWEEN 10 AND 100),
    reconciliation_pass boolean NOT NULL DEFAULT false,
    requested_at bigint NOT NULL CHECK(requested_at BETWEEN -62135596800000 AND 253402300799999),
    purge_after bigint NOT NULL CHECK(purge_after BETWEEN requested_at AND 253402300799999),
    completed_at bigint CHECK(completed_at BETWEEN -62135596800000 AND 253402300799999),
    next_attempt_at bigint NOT NULL CHECK(next_attempt_at BETWEEN -62135596800000 AND 253402300799999),
    attempts bigint NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 4294967295),
    last_error text CHECK(last_error='dataset_batch_failed'),
    expire_at bigint CHECK(expire_at BETWEEN -62135596800000 AND 253402300799999),
    CHECK((phase IN(0,1) AND completed_at IS NULL AND expire_at IS NULL) OR (phase IN(2,3) AND completed_at IS NOT NULL AND expire_at IS NOT NULL)),
    FOREIGN KEY(organization_id,project_id) REFERENCES metric.projects(organization_id,id)
);
CREATE UNIQUE INDEX deletion_one_active ON metric.project_deletions(project_id) WHERE phase IN(0,1);
CREATE INDEX deletion_due ON metric.project_deletions(next_attempt_at,operation_id) WHERE phase IN(0,1);
CREATE INDEX deletion_project_latest ON metric.project_deletions(project_id,requested_at DESC,operation_id DESC);
CREATE INDEX deletion_expiry ON metric.project_deletions(expire_at,operation_id) WHERE phase IN(2,3);
ALTER TABLE metric.artifact_bundles ADD COLUMN deletion_operation bytea CHECK(octet_length(deletion_operation)=16);
CREATE INDEX artifact_deletion_wait ON metric.artifact_bundles(deletion_operation,bundle_id) WHERE deletion_operation IS NOT NULL;
CREATE INDEX reserved_key_project_delete ON metric.reserved_keys(project_id,key);
CREATE INDEX deploy_binding_project_delete ON metric.deploy_projects(project_id,organization_id,deploy_id);
