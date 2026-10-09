CREATE TABLE metric.notification_project_locks(project_id integer PRIMARY KEY REFERENCES metric.projects(id));
CREATE TABLE metric.notification_destinations(
    destination_id bytea PRIMARY KEY CHECK(octet_length(destination_id)=16),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    kind smallint NOT NULL CHECK(kind BETWEEN 0 AND 2),
    endpoint bytea NOT NULL CHECK(octet_length(endpoint) BETWEEN 1 AND 2048),
    sealed_secret bytea NOT NULL CHECK(octet_length(sealed_secret) BETWEEN 1 AND 8192),
    config bytea NOT NULL CHECK(octet_length(config) BETWEEN 2 AND 65538),
    config_size integer NOT NULL CHECK(config_size BETWEEN 1 AND 65536),
    enabled boolean NOT NULL,
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    updated_at bigint NOT NULL CHECK(updated_at BETWEEN -62135596800000 AND 253402300799999)
);
CREATE INDEX notification_destination_page ON metric.notification_destinations(project_id,created_at,destination_id);
CREATE TABLE metric.notification_rules(
    rule_id bytea PRIMARY KEY CHECK(octet_length(rule_id)=16),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    kind smallint NOT NULL CHECK(kind BETWEEN 0 AND 2),
    trigger_mask smallint NOT NULL CHECK(trigger_mask BETWEEN 0 AND 7),
    name bytea NOT NULL CHECK(octet_length(name) BETWEEN 1 AND 200),
    enabled boolean NOT NULL,
    config bytea NOT NULL CHECK(octet_length(config) BETWEEN 2 AND 65538),
    config_size integer NOT NULL CHECK(config_size BETWEEN 1 AND 65536),
    cooldown_minutes integer NOT NULL CHECK(cooldown_minutes BETWEEN 0 AND 43200),
    storm_limit integer NOT NULL CHECK(storm_limit BETWEEN 1 AND 10000),
    next_evaluation_at bigint CHECK(next_evaluation_at BETWEEN -62135596800000 AND 253402300799999),
    lease_until bigint CHECK(lease_until BETWEEN -62135596800000 AND 253402300799999),
    last_triggered_at bigint CHECK(last_triggered_at BETWEEN -62135596800000 AND 253402300799999),
    storm_window_started_at bigint CHECK(storm_window_started_at BETWEEN -62135596800000 AND 253402300799999),
    storm_count bigint NOT NULL CHECK(storm_count BETWEEN 0 AND 4294967295),
    threshold_met boolean NOT NULL,
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    updated_at bigint NOT NULL CHECK(updated_at BETWEEN -62135596800000 AND 253402300799999),
    CHECK((kind=0)=(trigger_mask<>0)),
    CHECK(lease_until IS NULL OR kind=1)
);
CREATE INDEX notification_rule_page ON metric.notification_rules(project_id,created_at,rule_id);
CREATE INDEX notification_rule_match ON metric.notification_rules(project_id,rule_id) WHERE kind=0 AND enabled;
CREATE INDEX notification_rule_due ON metric.notification_rules(next_evaluation_at,rule_id) WHERE kind=1 AND enabled;
CREATE TABLE metric.notification_deliveries(
    delivery_id bytea PRIMARY KEY CHECK(octet_length(delivery_id)=16),
    project_id integer NOT NULL REFERENCES metric.projects(id),
    issue_id bytea NOT NULL CHECK(octet_length(issue_id)=16),
    transition_id bytea NOT NULL CHECK(octet_length(transition_id)=16),
    rule_id bytea NOT NULL CHECK(octet_length(rule_id)=16),
    action_id bytea NOT NULL CHECK(octet_length(action_id)=16),
    destination_id bytea NOT NULL CHECK(octet_length(destination_id)=16),
    payload bytea NOT NULL CHECK(octet_length(payload) BETWEEN 1 AND 16384),
    status smallint NOT NULL CHECK(status BETWEEN 0 AND 2),
    attempts bigint NOT NULL CHECK(attempts BETWEEN 0 AND 4294967295),
    next_attempt_at bigint CHECK(next_attempt_at BETWEEN -62135596800000 AND 253402300799999),
    attempted_at bigint CHECK(attempted_at BETWEEN -62135596800000 AND 253402300799999),
    last_error text CHECK(octet_length(last_error) BETWEEN 1 AND 64),
    created_at bigint NOT NULL CHECK(created_at BETWEEN -62135596800000 AND 253402300799999),
    terminal_at bigint CHECK(terminal_at BETWEEN -62135596800000 AND 253402300799999),
    delete_at bigint CHECK(delete_at BETWEEN -62135596800000 AND 253402300799999),
    CHECK((status=0)=(next_attempt_at IS NOT NULL)),
    CHECK((status=0)=(terminal_at IS NULL))
);
CREATE INDEX notification_delivery_due ON metric.notification_deliveries(next_attempt_at,delivery_id) WHERE status=0;
CREATE INDEX notification_delivery_page ON metric.notification_deliveries(project_id,created_at DESC,delivery_id DESC);
CREATE INDEX notification_delivery_expiry ON metric.notification_deliveries(delete_at,project_id,delivery_id) WHERE delete_at IS NOT NULL;
-- Cursor contains identifiers only. One bounded native lock serializes fairness
-- decisions across worker instances; project guards are always acquired first.
CREATE TABLE metric.notification_claim_cursor(
    singleton boolean PRIMARY KEY CHECK(singleton),
    last_project integer,
    last_destination bytea CHECK(octet_length(last_destination)=16),
    CHECK((last_project IS NULL)=(last_destination IS NULL))
);
INSERT INTO metric.notification_claim_cursor(singleton) VALUES(true);
