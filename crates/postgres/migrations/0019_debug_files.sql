ALTER TABLE metric.projects
    ADD COLUMN debug_file_bytes bigint NOT NULL DEFAULT 0 CHECK(debug_file_bytes>=0),
    ADD COLUMN debug_file_count bigint NOT NULL DEFAULT 0 CHECK(debug_file_count>=0);
CREATE TABLE metric.debug_uploads (
    upload_id bytea PRIMARY KEY CHECK(octet_length(upload_id)=16),
    project_id integer NOT NULL,
    organization_id bigint NOT NULL,
    codec_revision smallint NOT NULL DEFAULT 1 CHECK(codec_revision=1),
    sha1 bytea NOT NULL CHECK(octet_length(sha1)=20),
    name text NOT NULL CHECK(octet_length(name) BETWEEN 1 AND 255),
    debug_id bytea CHECK(octet_length(debug_id) IN(8,16,20)),
    code_id bytea CHECK(octet_length(code_id) BETWEEN 2 AND 65),
    chunks bytea NOT NULL CHECK(octet_length(chunks) BETWEEN 20 AND 5120 AND octet_length(chunks)%20=0),
    state smallint NOT NULL DEFAULT 0 CHECK(state BETWEEN 0 AND 3),
    attempts bigint NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 4294967295),
    error_code text CHECK(octet_length(error_code)<=64),
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    updated_at bigint NOT NULL CHECK(updated_at BETWEEN -62135596800000 AND 253402300799999),
    FOREIGN KEY(organization_id,project_id) REFERENCES metric.projects(organization_id,id)
);
CREATE INDEX debug_upload_project ON metric.debug_uploads(project_id,upload_id);
CREATE INDEX debug_upload_recovery ON metric.debug_uploads(updated_at,upload_id) WHERE state IN(0,1);
CREATE TABLE metric.debug_files (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    file_id bytea NOT NULL CHECK(octet_length(file_id)=16),
    codec_revision smallint NOT NULL DEFAULT 1 CHECK(codec_revision=1),
    debug_id bytea CHECK(octet_length(debug_id) IN(8,16,20)),
    code_id bytea CHECK(octet_length(code_id) BETWEEN 2 AND 65),
    file_type smallint NOT NULL CHECK(file_type BETWEEN 0 AND 5),
    checksum bytea NOT NULL CHECK(octet_length(checksum)=32),
    sha1 bytea NOT NULL CHECK(octet_length(sha1)=20),
    size bigint NOT NULL CHECK(size>=0),
    name text NOT NULL CHECK(octet_length(name) BETWEEN 1 AND 255),
    uploaded_at bigint NOT NULL CHECK(uploaded_at BETWEEN -62135596800000 AND 253402300799999),
    CHECK(debug_id IS NOT NULL OR code_id IS NOT NULL),
    PRIMARY KEY(project_id,file_id),
    UNIQUE(project_id,sha1)
);
CREATE INDEX debug_file_debug_lookup ON metric.debug_files(project_id,debug_id,uploaded_at DESC,file_id);
CREATE INDEX debug_file_code_lookup ON metric.debug_files(project_id,code_id,uploaded_at DESC,file_id);
