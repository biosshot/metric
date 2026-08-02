export type Permission =
  | 'event:read'
  | 'issue:read'
  | 'issue:write'
  | 'project:read'
  | 'project:admin'
  | 'release:read'
  | 'release:write'
  | 'organization:admin'
  | 'organization:owner'
  | 'organization:delete'
  | string;

export interface Identity {
  actor: 'web_session' | 'personal_api_token' | 'bootstrap';
  user_id: string;
  organization_id: string;
  role: 'owner' | 'admin' | 'member' | 'viewer';
  permissions: Permission[];
  credential_id: string;
}

export interface LoginResponse {
  csrf_token: string;
  expires_at: string;
  organization_id: string;
}

export interface ApiToken {
  id: string;
  name: string;
  scopes: string[];
  created_at: string;
  expires_at: string;
  last_used_at: string | null;
}

export interface CreatedApiToken {
  id: string;
  token: string;
  expires_at: string;
}

export interface Organization {
  id: string;
  slug: string;
  display_name: string;
  created_at: string;
}

export interface UserOrganization extends Organization {
  role: OrganizationRole;
}

export type OrganizationRole = 'owner' | 'admin' | 'member' | 'viewer';

export interface OrganizationMember {
  user_id: string;
  email: string;
  display_name: string;
  role: OrganizationRole;
  disabled_at: string | null;
  joined_at: string;
}

export interface OrganizationAuditRecord {
  request_id: string;
  actor: string;
  actor_user_id: string;
  action: string;
  target_kind: string;
  target_id: string;
  timestamp: string;
  metadata: Record<string, string>;
}

export interface CreatedInvitation {
  setup_token: string | null;
  organization_id: string;
  existing_account: boolean;
}

export interface ProjectPolicy {
  revision: number;
  ip_policy: 'hmac' | 'keep' | 'remove' | 'truncate';
  items: {
    error: boolean;
    client_report: boolean;
    log: boolean;
    transaction: boolean;
    span: boolean;
    feedback: boolean;
    check_in: boolean;
    metric: boolean;
    replay: boolean;
  };
  limits: {
    max_event_bytes: number;
    max_events_per_second: number | null;
    burst: number | null;
  };
  inbound_filters: InboundFilterRule[];
}

export type InboundFilterSignal = 'error' | 'log' | 'transaction' | 'span';
export type InboundFilterOperation = 'exact' | 'prefix' | 'suffix' | 'contains' | 'glob';
export type InboundFilterField =
  | 'release'
  | 'environment'
  | 'service'
  | 'message'
  | 'exception_type'
  | 'logger'
  | 'request_host'
  | 'request_path'
  | 'severity'
  | 'name'
  | 'operation'
  | 'status'
  | 'duration';

export interface InboundFilterRule {
  signal: InboundFilterSignal;
  field: InboundFilterField;
  operation: InboundFilterOperation;
  pattern: string;
}

export interface Project {
  id: string;
  organization_id: string;
  slug: string;
  display_name: string;
  state: 'active' | 'disabled' | 'pending_delete' | 'purging' | 'deleted';
  policy: ProjectPolicy;
  grouping_revision: number;
  created_at: string;
}

export interface CreateProjectInput {
  display_name: string;
  slug: string;
  ip_policy: ProjectPolicy['ip_policy'];
  error_enabled: boolean;
  client_report_enabled: boolean;
  log_enabled: boolean;
  transaction_enabled: boolean;
  span_enabled: boolean;
  feedback_enabled: boolean;
  check_in_enabled: boolean;
  metric_enabled: boolean;
  replay_enabled: boolean;
  max_event_bytes: number;
  max_events_per_second: number | null;
  burst: number | null;
}

export interface CreateProjectResponse {
  project_id: string;
  dsn_key: string;
}

export interface ProjectKey {
  dsn_key: string;
  project_id: string;
  state: 'active' | 'disabled' | 'suspended_by_deletion';
  label: string;
  created_at: string;
}

export interface ProjectDeletionStatus {
  operation_id: string;
  project_id: string;
  organization_id: string;
  phase: 'pending_grace' | 'purging' | 'deleted' | 'cancelled';
  dataset_code: number;
  reconciliation_pass: boolean;
  requested_at: string;
  purge_after: string;
  completed_at: string | null;
  next_attempt_at: string;
  attempts: number;
  last_error: string | null;
  status_url: string;
}

export interface Issue {
  id: string;
  project_id: string;
  title: string;
  culprit: string | null;
  status: 'open' | 'resolved' | 'ignored';
  first_seen: string;
  last_seen: string;
  first_event_id: string;
  latest_event_id: string;
  representative_event_id: string;
  occurrence_count: number;
  occurrence_count_approximate: boolean;
  assignee: { kind: string; id: string } | null;
  first_release: string | null;
  last_release: string | null;
  regression: {
    time: string;
    event_id: string;
    count: number;
    release: string | null;
  } | null;
  grouping: { strategy: string; summary: string };
}

export interface Event {
  event_id: string;
  project_id: string;
  issue_id: string;
  received_at: string;
  occurred_at: string;
  level: string;
  platform: string;
  body: Record<string, unknown>;
  replay_ids?: string[];
  feedback_ids?: string[];
}

export type LogLevel = 'trace' | 'debug' | 'info' | 'warn' | 'error' | 'fatal';

export interface StructuredLog {
  id: string;
  project_id: string;
  received_at: string;
  timestamp: string;
  timestamp_ns: string;
  level: LogLevel;
  message: string;
  trace_id: string | null;
  span_id: string | null;
  environment: string | null;
  release: string | null;
  service: string | null;
  body: Record<string, unknown>;
}

export interface Span {
  id: string;
  project_id: string;
  received_at: string;
  started_at: string;
  started_at_ns: string;
  ended_at: string;
  duration_ns: string;
  duration_ms: number;
  trace_id: string;
  span_id: string;
  parent_span_id: string | null;
  is_segment: boolean;
  operation_class: string;
  operation: string;
  status: string;
  name: string;
  environment: string | null;
  release: string | null;
  service: string | null;
  insight_flags: number;
  body: Record<string, unknown>;
}

export interface Trace {
  trace_id: string;
  spans: Span[];
  logs: StructuredLog[];
  errors: Array<{ event_id: string }>;
  replay_ids?: string[];
  feedback_ids?: string[];
  partial: boolean;
  omitted_spans: number;
}

export interface PerformanceBucket {
  hour: string;
  name: string;
  service: string | null;
  environment: string | null;
  release: string | null;
  representative_trace_id: string;
  operation: string;
  count: number;
  failure_count: number;
  failure_rate: number;
  average_duration_ms: number;
  p50_ms: number;
  p75_ms: number;
  p90_ms: number;
  p95_ms: number;
  p99_ms: number;
  approximate: true;
  sample_limit: number;
}

export type ExploreDataset = 'errors' | 'logs' | 'spans' | 'metrics';
export type ExploreShape = 'table' | 'number' | 'timeseries';
export type ExploreScalar = string | number | boolean | null;

export interface ExploreRequest {
  dataset: ExploreDataset;
  from: number;
  until: number;
  query?: string;
  predicates: Array<{
    field: string;
    op: 'exact' | 'contains' | 'starts_with' | 'ends_with' | 'present' | 'range';
    value?: ExploreScalar;
    upper?: ExploreScalar;
  }>;
  aggregates: Array<{
    function: 'count' | 'sum' | 'min' | 'max' | 'avg' | 'p50' | 'p75' | 'p90' | 'p95' | 'p99';
    field?: string;
    alias?: string;
  }>;
  group_by: string[];
  interval?: '1m' | '5m' | '1h' | '1d';
  cursor?: string | null;
  limit: number;
}

export type QuerySource =
  | 'issues'
  | 'errors'
  | 'logs'
  | 'traces'
  | 'metrics'
  | 'replays'
  | 'feedback'
  | 'releases';

export type QueryAggregate = ExploreRequest['aggregates'][number];

export type UnifiedQueryResultSpec =
  | { kind: 'records' }
  | { kind: 'number'; aggregates: QueryAggregate[]; group_by?: string[] }
  | {
      kind: 'timeseries';
      aggregates: QueryAggregate[];
      group_by?: string[];
      interval: '1m' | '5m' | '1h' | '1d';
    }
  | { kind: 'values'; field: string };

export interface UnifiedQueryRequest {
  source: QuerySource;
  query: string;
  from?: number;
  until?: number;
  result: UnifiedQueryResultSpec;
  cursor?: string | null;
  limit?: number;
  output?: { kind: 'download'; format: 'json' | 'csv' };
}

export interface UnifiedQueryResult<T = Record<string, ExploreScalar>> {
  source: QuerySource;
  kind: 'records' | 'number' | 'timeseries' | 'values';
  items: T[];
  next_cursor: string | null;
  normalized_query: string;
  cost: number;
  field?: string;
  candidates_examined?: number;
}

export interface SavedQuery {
  id: string;
  project_id: string;
  name: string;
  query: ExploreRequest;
  revision: number;
  created_by: string;
  updated_by: string;
  created_at: number;
  updated_at: number;
}

export interface DashboardWidget {
  id: string;
  title: string;
  saved_query_id: string;
  shape: ExploreShape;
}

export interface Dashboard {
  id: string;
  project_id: string;
  name: string;
  widgets: DashboardWidget[];
  refresh_interval: 'manual' | '30s' | '1m' | '5m';
  revision: number;
  created_by: string;
  updated_by: string;
  created_at: number;
  updated_at: number;
}

export interface DashboardRefresh {
  dashboard_id: string;
  refreshed_at: number;
  total_cost: number;
  widgets: Array<{
    widget_id: string;
    cost: number | null;
    error_code: string | null;
    items: Array<Record<string, ExploreScalar>> | null;
  }>;
}

export type NotificationDestinationKind = 'telegram' | 'smtp_email';

export interface NotificationDestination {
  id: string;
  project_id: string;
  kind: NotificationDestinationKind;
  endpoint: string;
  has_secret: true;
  smtp: {
    port: number;
    security: 'starttls' | 'tls';
    username: string;
    from: string;
    recipients: string[];
  } | null;
  enabled: boolean;
  created_at: number;
  updated_at: number;
}

export interface TelegramBot {
  id: string;
  username: string;
  display_name: string;
}

export interface TelegramSubscriberSync {
  bot: TelegramBot;
  subscribers: Array<{
    destination_id: string;
    display_name: string;
  }>;
}

export interface AlertRule {
  id: string;
  project_id: string;
  name: string;
  enabled: boolean;
  triggers: Array<'new_issue' | 'regression' | 'resolved'>;
  aggregate: {
    dataset: ExploreDataset;
    lookback_minutes: number;
    evaluation_interval_minutes: number;
    threshold: number;
    environment: string | null;
    release: string | null;
    notify_resolved: boolean;
  } | null;
  monitor: {
    monitor_id: string;
    outcomes: Array<'error' | 'timeout' | 'missed'>;
    notify_resolved: boolean;
  } | null;
  destination_ids: string[];
  cooldown_minutes: number;
  storm_limit_per_hour: number;
  created_at: number;
  updated_at: number;
}

export type MonitorRunStatus = 'in_progress' | 'success' | 'error' | 'timeout' | 'missed';

export interface CronMonitor {
  id: string;
  project_id: string;
  slug: string;
  name: string;
  environment: string;
  enabled: boolean;
  managed_by: 'web' | 'sdk';
  revision: number;
  kind: 'cron' | 'uptime';
  schedule_type: 'interval' | 'crontab';
  schedule: string;
  checkin_margin_seconds: number;
  max_runtime_seconds: number;
  next_expected_at: string;
  last_run_id: string | null;
  last_status: MonitorRunStatus | null;
  last_check_in_at: string | null;
  created_at: string;
  updated_at: string;
  uptime: {
    endpoint: string;
    method: 'GET' | 'HEAD';
    expected_status_min: number;
    expected_status_max: number;
    timeout_seconds: number;
    max_redirects: number;
    headers: Array<{ name: string; sensitive: boolean; has_value: boolean }>;
  } | null;
}

export interface MonitorRun {
  id: string;
  monitor_id: string;
  status: MonitorRunStatus;
  source: 'sdk' | 'scheduler';
  scheduled_for: string | null;
  started_at: string;
  finished_at: string | null;
  duration_ms: number | null;
  received_at: string;
  release_id: string | null;
  http_status: number | null;
  uptime_failure: string | null;
}

export interface MonitorInput {
  kind: 'cron' | 'uptime';
  slug: string;
  name: string;
  environment: string;
  enabled: boolean;
  schedule_type: 'interval' | 'crontab';
  schedule: string;
  checkin_margin_seconds: number;
  max_runtime_seconds: number;
  endpoint: string | null;
  method: 'GET' | 'HEAD' | null;
  expected_status_min: number | null;
  expected_status_max: number | null;
  timeout_seconds: number | null;
  max_redirects: number | null;
  headers: Array<{ name: string; value: string }>;
}

export interface NotificationDelivery {
  id: string;
  destination_id: string;
  status: 'pending' | 'delivered' | 'dead';
  attempts: number;
  last_error: string | null;
  created_at: number;
  delivered_at: number | null;
}

export type FeedbackStatus = 'open' | 'resolved' | 'spam';

export interface FeedbackAttachment {
  attachment_id: string;
  filename: string;
  content_type: string;
  attachment_type: string;
  size: number;
  checksum: string;
}

export interface Feedback {
  id: string;
  project_id: string;
  received_at: string;
  status: FeedbackStatus;
  status_changed_at: string;
  message: string;
  name: string | null;
  contact_email: string | null;
  url: string | null;
  associated_event_id: string | null;
  issue_id: string | null;
  trace_id: string | null;
  replay_id: string | null;
  attachments: FeedbackAttachment[];
  expires_at: string;
}

export interface ReplaySegment {
  segment_id: number;
  size: number;
  decompressed_bytes: number;
  event_count: number;
  checksum: string;
}

export interface Replay {
  id: string;
  project_id: string;
  started_at: string;
  ended_at: string;
  received_at: string;
  duration_ms: number;
  environment: string | null;
  release: string | null;
  url: string | null;
  error_ids: string[];
  trace_ids: string[];
  feedback_ids?: string[];
  segments: ReplaySegment[];
  partial: boolean;
  expires_at: string | null;
}

export interface ReleaseSummary {
  id: string;
  version: string;
  activity_at: string;
  first_seen: string | null;
  last_seen: string | null;
  released_at: string | null;
  explicit: boolean;
}

export interface Release extends ReleaseSummary {
  project_ids: string[];
  created_at: string;
  first_event_id: string | null;
  latest_event_id: string | null;
  url: string | null;
  reference: string | null;
  repositories: Array<{
    repository: string;
    commit_from: string | null;
    commit_to: string | null;
  }>;
}

export interface Deploy {
  id: string;
  release_id: string;
  environment: string;
  name: string | null;
  url: string | null;
  started_at: string;
  finished_at: string | null;
  created_at: string;
}

export interface ReleaseIssue {
  id: string;
  title: string;
  first_seen: string;
  last_seen: string;
  first_release: string | null;
  last_release: string | null;
}

export interface ReleaseHealthBucket {
  hour: string;
  environment_id: string;
  environment: string;
  sessions: number;
  crashed: number;
  abnormal: number;
  exited: number;
  crash_free_sessions: number;
  approximate_users: number;
  approximate_crashed_users: number;
  crash_free_users: number;
}

export interface ReleaseHealth {
  items: ReleaseHealthBucket[];
  approximate_users: true;
  users: number;
  crashed_users: number;
  crash_free_users: number;
  user_sketch_bytes: number;
  user_sketch_standard_error_percent: number;
  user_sketch_saturation_estimate: number;
}

export interface IssueStatistic {
  bucket_start: string;
  occurrence_count: number;
  approximate: boolean;
}

export interface IssueActivity {
  id: string;
  issue_id: string;
  kind: string;
  actor: { kind: string; id: string };
  event_id: string | null;
  at: string;
}

export interface CapabilityDocument {
  api_version: string;
  search: {
    fields: string[];
    full_text: boolean;
    custom_tags: boolean;
    max_page_size: number;
  };
  features: Record<string, boolean>;
  query_export: {
    formats: Array<'json' | 'csv'>;
    max_rows: number;
    max_bytes: number;
    max_duration_seconds: number;
    max_concurrency: number;
  };
  retention: {
    events_days: number;
    feedback_days: number;
    issue_stats_hourly_days: number;
    logs_days: number;
    spans_days: number;
    span_stats_hourly_days: number;
    sessions_days: number;
    session_stats_hourly_days: number;
    session_active_max_hours: number;
    monitor_runs_days: number;
    clock: 'received_at';
    gradual_policy_reduction: boolean;
  } | null;
  project_deletion: {
    grace_period_seconds: number;
    delete_batch_documents: number;
    slug_reservation_seconds: number;
    final_reconciliation: boolean;
    filesystem_namespaces: number;
  } | null;
  explore: {
    datasets: ExploreDataset[];
    maximum_range_days: number;
    maximum_predicates: number;
    maximum_group_by: number;
    maximum_rows: number;
    intervals: string[];
    raw_database_syntax: false;
  };
  dashboards: {
    maximum_widgets: number;
    maximum_total_cost: number;
    maximum_refresh_concurrency: number;
    refresh_intervals: string[];
    variables: string[];
    result_cache: false;
  };
}

export interface ComponentStatus {
  status: 'ready' | 'degraded';
  components: Record<string, string>;
}

export interface Page<T> {
  items: T[];
  next_cursor: string | null;
}

export interface ApiErrorBody {
  error: {
    code: string;
    message: string;
    request_id: string;
    details?: Record<string, unknown>;
  };
}
