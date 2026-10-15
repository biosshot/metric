ALTER TABLE metric.organizations
    ADD COLUMN artifact_bytes bigint NOT NULL DEFAULT 0 CHECK(artifact_bytes>=0),
    ADD COLUMN artifact_count bigint NOT NULL DEFAULT 0 CHECK(artifact_count>=0);
CREATE TABLE metric.artifact_uploads (
    upload_id bytea PRIMARY KEY CHECK(octet_length(upload_id)=16),
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    codec_revision smallint NOT NULL DEFAULT 1 CHECK(codec_revision=1),
    sha1 bytea NOT NULL CHECK(octet_length(sha1)=20),
    chunks bytea NOT NULL CHECK(octet_length(chunks) BETWEEN 20 AND 1280 AND octet_length(chunks)%20=0),
    state smallint NOT NULL DEFAULT 0 CHECK(state BETWEEN 0 AND 3),
    attempts bigint NOT NULL DEFAULT 0 CHECK(attempts BETWEEN 0 AND 4294967295),
    final_id bytea CHECK(octet_length(final_id)=16),
    error_code integer CHECK(error_code BETWEEN 1 AND 65535),
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    updated_at bigint NOT NULL CHECK(updated_at BETWEEN -62135596800000 AND 253402300799999),
    expire_at bigint CHECK(expire_at BETWEEN -62135596800000 AND 253402300799999),
    UNIQUE(organization_id,upload_id), UNIQUE(organization_id,sha1)
);
CREATE INDEX artifact_upload_recovery ON metric.artifact_uploads(updated_at,upload_id) WHERE state IN(0,1);
CREATE INDEX artifact_upload_sha1 ON metric.artifact_uploads(organization_id,sha1,upload_id);
CREATE INDEX artifact_upload_expiry ON metric.artifact_uploads(expire_at,upload_id) WHERE expire_at IS NOT NULL;
CREATE TABLE metric.artifact_bundles (
    bundle_id bytea PRIMARY KEY CHECK(octet_length(bundle_id)=16),
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    codec_revision smallint NOT NULL DEFAULT 1 CHECK(codec_revision=1),
    sha1 bytea NOT NULL CHECK(octet_length(sha1)=20),
    bundle_debug_id bytea NOT NULL CHECK(octet_length(bundle_debug_id) IN(8,16,20)),
    debug_tokens bigint[] NOT NULL CHECK(cardinality(debug_tokens)<=20000 AND array_position(debug_tokens,NULL) IS NULL),
    checksum bytea NOT NULL CHECK(octet_length(checksum)=32),
    size bigint NOT NULL CHECK(size>=0),
    uploaded_at bigint NOT NULL CHECK(uploaded_at BETWEEN -62135596800000 AND 253402300799999),
    generation bigint NOT NULL DEFAULT 0 CHECK(generation BETWEEN 0 AND 4294967295),
    state smallint NOT NULL DEFAULT 0 CHECK(state BETWEEN 0 AND 3),
    due_at bigint CHECK(due_at BETWEEN -62135596800000 AND 253402300799999),
    claim bytea CHECK(octet_length(claim)=16),
    CHECK((state=0 AND claim IS NULL) OR (state=1 AND claim IS NOT NULL AND due_at IS NOT NULL) OR (state IN(2,3) AND claim IS NULL AND due_at IS NOT NULL)),
    UNIQUE(organization_id,sha1), UNIQUE(organization_id,bundle_id)
);
CREATE INDEX artifact_debug_tokens ON metric.artifact_bundles USING gin(debug_tokens) WHERE state=0;
CREATE INDEX artifact_organization_timeline ON metric.artifact_bundles(organization_id,uploaded_at DESC,bundle_id DESC) WHERE state=0;
CREATE INDEX artifact_gc_due ON metric.artifact_bundles(due_at,bundle_id) WHERE state IN(0,1) AND due_at IS NOT NULL;
CREATE INDEX artifact_tombstone_expiry ON metric.artifact_bundles(due_at,bundle_id) WHERE state=2;
CREATE TABLE metric.artifact_upload_bindings (
    organization_id bigint NOT NULL,
    upload_id bytea NOT NULL,
    project_id integer NOT NULL,
    release_id bytea CHECK(octet_length(release_id)=16),
    dist text CHECK(dist IS NULL OR (octet_length(dist) BETWEEN 1 AND 64 AND release_id IS NOT NULL)),
    UNIQUE NULLS NOT DISTINCT(organization_id,upload_id,project_id,release_id,dist),
    FOREIGN KEY(organization_id,upload_id) REFERENCES metric.artifact_uploads(organization_id,upload_id) ON DELETE CASCADE,
    FOREIGN KEY(organization_id,project_id) REFERENCES metric.projects(organization_id,id)
);
CREATE INDEX artifact_upload_binding_project ON metric.artifact_upload_bindings(project_id,organization_id,upload_id);
CREATE TABLE metric.artifact_bundle_bindings (
    organization_id bigint NOT NULL,
    bundle_id bytea NOT NULL,
    project_id integer NOT NULL,
    release_id bytea CHECK(octet_length(release_id)=16),
    dist text CHECK(dist IS NULL OR (octet_length(dist) BETWEEN 1 AND 64 AND release_id IS NOT NULL)),
    UNIQUE NULLS NOT DISTINCT(organization_id,bundle_id,project_id,release_id,dist),
    FOREIGN KEY(organization_id,bundle_id) REFERENCES metric.artifact_bundles(organization_id,bundle_id) ON DELETE CASCADE,
    FOREIGN KEY(organization_id,project_id) REFERENCES metric.projects(organization_id,id)
);
CREATE INDEX artifact_project_release ON metric.artifact_bundle_bindings(organization_id,project_id,release_id,dist,bundle_id);
CREATE INDEX artifact_binding_project ON metric.artifact_bundle_bindings(project_id,organization_id,bundle_id);
