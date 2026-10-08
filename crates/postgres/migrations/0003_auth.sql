CREATE TABLE metric.users (
    id bigint PRIMARY KEY CHECK (id>0),
    email text NOT NULL CHECK (octet_length(email) BETWEEN 1 AND 254),
    canonical_email text NOT NULL CHECK (canonical_email=lower(email)),
    display_name text NOT NULL CHECK (octet_length(display_name) BETWEEN 1 AND 128),
    password_hash text CHECK (octet_length(password_hash) BETWEEN 1 AND 512),
    disabled_at bigint CHECK (disabled_at BETWEEN -62135596800000 AND 253402300799999),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    password_changed_at bigint CHECK (password_changed_at BETWEEN -62135596800000 AND 253402300799999),
    CONSTRAINT user_canonical_email_unique UNIQUE (canonical_email)
);
CREATE TABLE metric.organization_memberships (
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    user_id bigint NOT NULL REFERENCES metric.users(id),
    role smallint NOT NULL CHECK (role BETWEEN 0 AND 3),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    created_by bigint NOT NULL REFERENCES metric.users(id),
    PRIMARY KEY (organization_id,user_id)
);
CREATE INDEX membership_user_list ON metric.organization_memberships(user_id,organization_id);
CREATE INDEX membership_organization_list ON metric.organization_memberships(organization_id,created_at,user_id);
CREATE TABLE metric.setup_tokens (
    id bigint PRIMARY KEY CHECK (id>0),
    digest bytea NOT NULL UNIQUE CHECK (octet_length(digest)=32),
    purpose smallint NOT NULL CHECK (purpose BETWEEN 0 AND 2),
    user_id bigint REFERENCES metric.users(id),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    expires_at bigint NOT NULL CHECK (expires_at BETWEEN -62135596800000 AND 253402300799999),
    consumed_at bigint CHECK (consumed_at BETWEEN -62135596800000 AND 253402300799999),
    operation_id bigint CHECK (operation_id>0),
    CHECK ((purpose=0 AND user_id IS NULL) OR (purpose<>0 AND user_id IS NOT NULL))
);
CREATE UNIQUE INDEX setup_bootstrap_singleton ON metric.setup_tokens(purpose) WHERE purpose=0;
CREATE UNIQUE INDEX setup_active_per_user ON metric.setup_tokens(user_id) WHERE purpose=1 AND consumed_at IS NULL;
CREATE INDEX setup_token_expiry ON metric.setup_tokens(expires_at);
CREATE TABLE metric.web_sessions (
    id bigint PRIMARY KEY CHECK (id>0),
    digest bytea NOT NULL UNIQUE CHECK (octet_length(digest)=32),
    csrf_digest bytea NOT NULL CHECK (octet_length(csrf_digest)=32),
    user_id bigint NOT NULL REFERENCES metric.users(id),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    last_seen_at bigint NOT NULL CHECK (last_seen_at BETWEEN -62135596800000 AND 253402300799999),
    idle_expires_at bigint NOT NULL CHECK (idle_expires_at BETWEEN -62135596800000 AND 253402300799999),
    absolute_expires_at bigint NOT NULL CHECK (absolute_expires_at BETWEEN -62135596800000 AND 253402300799999),
    revoked_at bigint CHECK (revoked_at BETWEEN -62135596800000 AND 253402300799999)
);
CREATE INDEX session_user_active ON metric.web_sessions(user_id) WHERE revoked_at IS NULL;
CREATE INDEX session_expiry ON metric.web_sessions(absolute_expires_at);
CREATE TABLE metric.api_tokens (
    id bigint PRIMARY KEY CHECK (id>0),
    digest bytea NOT NULL UNIQUE CHECK (octet_length(digest)=32),
    user_id bigint NOT NULL REFERENCES metric.users(id),
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    name text NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 64),
    scopes integer NOT NULL CHECK (scopes BETWEEN 0 AND 131071),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    expires_at bigint NOT NULL CHECK (expires_at BETWEEN -62135596800000 AND 253402300799999),
    last_used_at bigint CHECK (last_used_at BETWEEN -62135596800000 AND 253402300799999),
    revoked_at bigint CHECK (revoked_at BETWEEN -62135596800000 AND 253402300799999)
);
CREATE INDEX api_token_user_list ON metric.api_tokens(user_id,organization_id,created_at DESC,id DESC) WHERE revoked_at IS NULL;
CREATE INDEX api_token_expiry ON metric.api_tokens(expires_at);
CREATE TABLE metric.audit_log (
    request_id text PRIMARY KEY CHECK (octet_length(request_id) BETWEEN 1 AND 64),
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    actor smallint NOT NULL CHECK (actor BETWEEN 0 AND 2),
    actor_user_id bigint NOT NULL REFERENCES metric.users(id),
    action text NOT NULL CHECK (octet_length(action) BETWEEN 1 AND 128),
    target_kind text NOT NULL CHECK (octet_length(target_kind) BETWEEN 1 AND 128),
    target_id text NOT NULL CHECK (octet_length(target_id) BETWEEN 1 AND 128),
    timestamp bigint NOT NULL CHECK (timestamp BETWEEN -62135596800000 AND 253402300799999),
    metadata jsonb NOT NULL CHECK (jsonb_typeof(metadata)='array' AND jsonb_array_length(metadata)<=12 AND octet_length(metadata::text)<=8192)
);
CREATE INDEX audit_organization_timeline ON metric.audit_log(organization_id,timestamp DESC,request_id DESC);
