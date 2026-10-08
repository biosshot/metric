-- Saved Queries and Dashboards share storage mechanics, not domain contracts.
CREATE TABLE metric.dashboard_configurations (
    project_id integer NOT NULL REFERENCES metric.projects(id),
    kind smallint NOT NULL CHECK (kind IN (0,1)),
    configuration_id bytea NOT NULL CHECK (octet_length(configuration_id)=16),
    name text NOT NULL CHECK (octet_length(name) BETWEEN 1 AND 120),
    revision bigint NOT NULL CHECK (revision>0),
    created_by bigint NOT NULL CHECK (created_by>0),
    updated_by bigint NOT NULL CHECK (updated_by>0),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    updated_at bigint NOT NULL CHECK (updated_at BETWEEN -62135596800000 AND 253402300799999 AND updated_at>=created_at),
    body bytea NOT NULL CHECK (octet_length(body) BETWEEN 2 AND 524290),
    PRIMARY KEY (project_id,kind,configuration_id),
    UNIQUE (project_id,kind,name)
);
CREATE INDEX dashboard_configuration_timeline ON metric.dashboard_configurations(project_id,kind,updated_at DESC,configuration_id DESC);
