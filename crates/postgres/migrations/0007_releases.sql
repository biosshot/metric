-- Match the public Release-Issue cursor: first occurrence for new Issues,
-- receipt-time regression for regressed Issues, rather than last occurrence.
DROP INDEX metric.issue_release_new_timeline;
DROP INDEX metric.issue_release_regression_timeline;
CREATE INDEX issue_release_new_timeline ON metric.issues(project_id,first_release,first_seen DESC,issue_id DESC) WHERE first_release IS NOT NULL;
CREATE INDEX issue_release_regression_timeline ON metric.issues(project_id,regression_release,regression_at DESC,issue_id DESC) WHERE regression_release IS NOT NULL;
CREATE TABLE metric.deploys (
    organization_id bigint NOT NULL,
    deploy_id bytea NOT NULL CHECK (octet_length(deploy_id)=16),
    release_id bytea NOT NULL CHECK (octet_length(release_id)=16),
    environment bytea NOT NULL CHECK (octet_length(environment) BETWEEN 1 AND 64),
    name bytea CHECK (octet_length(name) BETWEEN 1 AND 200),
    url bytea CHECK (octet_length(url) BETWEEN 1 AND 2048),
    started_at bigint NOT NULL CHECK (started_at BETWEEN -62135596800000 AND 253402300799999),
    finished_at bigint CHECK (finished_at BETWEEN -62135596800000 AND 253402300799999 AND finished_at>=started_at),
    created_at bigint NOT NULL CHECK (created_at BETWEEN -62135596800000 AND 253402300799999),
    PRIMARY KEY (organization_id,deploy_id),
    FOREIGN KEY (organization_id,release_id) REFERENCES metric.releases(organization_id,release_id),
    UNIQUE (organization_id,deploy_id,release_id,started_at)
);
CREATE TABLE metric.deploy_projects (
    organization_id bigint NOT NULL,
    deploy_id bytea NOT NULL,
    release_id bytea NOT NULL,
    started_at bigint NOT NULL,
    project_id integer NOT NULL,
    ordinal smallint NOT NULL CHECK (ordinal BETWEEN 0 AND 255),
    PRIMARY KEY (organization_id,deploy_id,project_id),
    UNIQUE (organization_id,deploy_id,ordinal),
    FOREIGN KEY (organization_id,deploy_id,release_id,started_at) REFERENCES metric.deploys(organization_id,deploy_id,release_id,started_at),
    FOREIGN KEY (organization_id,project_id) REFERENCES metric.projects(organization_id,id)
);
CREATE INDEX deploy_project_release_timeline ON metric.deploy_projects(organization_id,project_id,release_id,started_at DESC,deploy_id DESC);
