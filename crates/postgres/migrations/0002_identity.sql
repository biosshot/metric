CREATE TABLE metric.organizations (
    id bigint PRIMARY KEY CHECK (id > 0),
    slug text NOT NULL CHECK (octet_length(slug) BETWEEN 1 AND 63),
    display_name text NOT NULL CHECK (octet_length(display_name) BETWEEN 1 AND 128),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    CONSTRAINT organization_slug_unique UNIQUE (slug)
);
CREATE TABLE metric.projects (
    id integer PRIMARY KEY CHECK (id > 0),
    organization_id bigint NOT NULL REFERENCES metric.organizations(id),
    slug text NOT NULL CHECK (octet_length(slug) BETWEEN 1 AND 63),
    display_name text NOT NULL CHECK (octet_length(display_name) BETWEEN 1 AND 128),
    state smallint NOT NULL CHECK (state BETWEEN 0 AND 4),
    policy_revision bigint NOT NULL CHECK (policy_revision > 0),
    ip_policy smallint NOT NULL CHECK (ip_policy BETWEEN 0 AND 3),
    items integer NOT NULL CHECK (items BETWEEN 0 AND 511),
    max_event_bytes bigint NOT NULL CHECK (max_event_bytes BETWEEN 1 AND 4294967295),
    max_events_per_second bigint CHECK (max_events_per_second BETWEEN 1 AND 4294967295),
    burst bigint CHECK (burst BETWEEN 1 AND 4294967295),
    inbound_filters jsonb NOT NULL DEFAULT '[]' CHECK (jsonb_typeof(inbound_filters)='array' AND jsonb_array_length(inbound_filters)<=32 AND octet_length(inbound_filters::text)<=65536),
    grouping_revision bigint NOT NULL CHECK (grouping_revision > 0),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    CONSTRAINT project_organization_slug_unique UNIQUE (organization_id, slug)
);
CREATE INDEX project_organization_list ON metric.projects(organization_id, slug, id) WHERE state<>4;

-- A single reservation namespace prevents alias/native-key races, including disabled keys.
CREATE TABLE metric.reserved_keys (
    key bytea PRIMARY KEY CHECK (octet_length(key)=16),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    UNIQUE (key, project_id)
);
CREATE TABLE metric.project_keys (
    key bytea PRIMARY KEY,
    project_id integer NOT NULL,
    state smallint NOT NULL CHECK (state BETWEEN 0 AND 2),
    label text NOT NULL CHECK (octet_length(label) BETWEEN 1 AND 64),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    FOREIGN KEY (key, project_id) REFERENCES metric.reserved_keys(key, project_id),
    UNIQUE (project_id, key)
);
CREATE INDEX project_key_list ON metric.project_keys(project_id, created_at, key);
CREATE TABLE metric.dsn_mappings (
    source_key bytea PRIMARY KEY,
    project_id integer NOT NULL,
    target_key bytea NOT NULL,
    dsn text NOT NULL CHECK (octet_length(dsn) BETWEEN 1 AND 2048),
    FOREIGN KEY (source_key, project_id) REFERENCES metric.reserved_keys(key, project_id),
    FOREIGN KEY (project_id, target_key) REFERENCES metric.project_keys(project_id, key),
    UNIQUE (project_id, target_key)
);
