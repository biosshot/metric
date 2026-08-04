import type {
  ApiErrorBody,
  ApiToken,
  CapabilityDocument,
  ComponentStatus,
  CreatedInvitation,
  CreatedApiToken,
  Dashboard,
  DashboardRefresh,
  AlertRule,
  CronMonitor,
  CreateProjectInput,
  CreateProjectResponse,
  Event,
  ExploreRequest,
  UnifiedQueryRequest,
  UnifiedQueryResult,
  Feedback,
  FeedbackStatus,
  Identity,
  Issue,
  IssueActivity,
  IssueStatistic,
  LoginResponse,
  NotificationDestination,
  NotificationDelivery,
  TelegramBot,
  TelegramSubscriberSync,
  MonitorInput,
  MonitorRun,
  Organization,
  OrganizationAuditRecord,
  OrganizationMember,
  OrganizationRole,
  Page,
  PerformanceBucket,
  Project,
  ProjectKey,
  ProjectDeletionStatus,
  ProjectPolicy,
  Release,
  ReleaseIssue,
  ReleaseHealth,
  Replay,
  SavedQuery,
  Deploy,
  StructuredLog,
  Trace,
  UserOrganization,
} from './types';
import { queryForBackend } from '../lib/queryDate';
import { randomHexId } from '../lib/randomId';

type SessionProvider = () => { organizationId: string | null; csrfToken: string | null };
type SessionInvalidator = () => void;

const messages: Record<string, string> = {
  invalid_credentials: 'The session or credentials are no longer valid.',
  csrf_failed: 'The security token expired. Sign in again before changing data.',
  forbidden: 'Your role does not allow this action.',
  invalid_request: 'The request contains invalid values.',
  invalid_cursor: 'This page link expired. Return to the first page.',
  not_found: 'The requested object no longer exists.',
  conflict: 'The object changed. Refresh it before trying again.',
  rate_limited: 'Too many requests. Wait briefly and retry.',
  temporarily_unavailable: 'A required Metric component is unavailable.',
  query_syntax_invalid: 'The query expression is not valid.',
  query_capability_unavailable: 'This field or result is not available for the selected source.',
  query_limit_exceeded: 'The query is too complex. Remove some conditions.',
  query_requires_positive_anchor: 'Add a positive indexed condition or a bounded time range.',
  query_too_broad: 'The query examined too many candidates. Add another condition.',
  query_cost_exceeded: 'The query is too expensive. Shorten the range or grouping.',
  query_capacity: 'Query capacity is busy. Wait briefly and retry.',
  query_unavailable: 'Query storage is temporarily unavailable.',
  explore_invalid_query: 'This Explore query uses an unsupported field or combination.',
  explore_cost_exceeded: 'This Explore query is too expensive. Shorten the range or grouping.',
  explore_capacity: 'Explore is busy. Wait briefly and retry.',
  explore_unavailable: 'Explore storage is temporarily unavailable.',
  dashboard_invalid_request: 'The saved query or dashboard configuration is invalid.',
  dashboard_not_found: 'The saved query or dashboard no longer exists.',
  dashboard_conflict: 'This shared dashboard changed. Reload it before saving.',
  dashboard_cost_exceeded: 'This dashboard exceeds its total query-cost budget.',
  dashboard_capacity: 'Dashboard refresh capacity is busy. Wait briefly and retry.',
  dashboard_unavailable: 'Dashboard storage is temporarily unavailable.',
};

export class ApiError extends Error {
  constructor(
    public readonly status: number,
    public readonly code: string,
    public readonly requestId: string | null,
    message: string,
    public readonly retryable = false,
  ) {
    super(message);
    this.name = 'ApiError';
  }
}

let sessionProvider: SessionProvider = () => ({ organizationId: null, csrfToken: null });
let invalidateSession: SessionInvalidator = () => undefined;

export function configureSession(
  provider: SessionProvider,
  invalidator: SessionInvalidator = () => undefined,
): void {
  sessionProvider = provider;
  invalidateSession = invalidator;
}

function isMutation(method: string): boolean {
  return !['GET', 'HEAD', 'OPTIONS'].includes(method);
}

async function request<T>(
  path: string,
  init: RequestInit = {},
  options: { public?: boolean; organizationId?: string } = {},
): Promise<T> {
  const method = (init.method ?? 'GET').toUpperCase();
  const session = sessionProvider();
  const headers = new Headers(init.headers);
  headers.set('accept', 'application/json');
  if (init.body) headers.set('content-type', 'application/json');
  const organizationId = options.organizationId ?? session.organizationId;
  if (!options.public && organizationId) {
    headers.set('x-metric-organization-id', organizationId);
  }
  if (!options.public && isMutation(method)) {
    if (!session.csrfToken) {
      const error = new ApiError(
        403,
        'csrf_missing',
        null,
        'This tab cannot safely change data. Sign in again to restore the security token.',
      );
      invalidateSession();
      throw error;
    }
    headers.set('x-csrf-token', session.csrfToken);
  }

  let response: Response;
  try {
    response = await fetch(path, { ...init, method, headers, credentials: 'include' });
  } catch {
    throw new ApiError(
      0,
      'network_error',
      null,
      'Cannot reach Metric. Check the connection and server status.',
      true,
    );
  }

  if (!response.ok) {
    const error = await responseError(response);
    if (!options.public && invalidatesSession(error)) invalidateSession();
    throw error;
  }
  if (response.status === 204) return undefined as T;
  return (await response.json()) as T;
}

function query(values: Record<string, string | number | null | undefined>): string {
  const params = new URLSearchParams();
  for (const [name, value] of Object.entries(values)) {
    if (value !== null && value !== undefined && value !== '') params.set(name, String(value));
  }
  const encoded = params.toString();
  return encoded ? `?${encoded}` : '';
}

function queryTimestamp(value: number | undefined): string | undefined {
  return value === undefined ? undefined : new Date(value).toISOString();
}

function exploreRequestForBackend(value: ExploreRequest): ExploreRequest {
  return value.query ? { ...value, query: queryForBackend(value.query) } : value;
}

async function binaryRequest(path: string): Promise<ArrayBuffer> {
  const headers = authenticatedHeaders();
  headers.set('accept', 'application/vnd.sentry.items.replay-recording');
  let response: Response;
  try {
    response = await fetch(path, { headers, credentials: 'include' });
  } catch {
    throw new ApiError(
      0,
      'network_error',
      null,
      'Cannot reach Metric. Check the connection and server status.',
      true,
    );
  }
  if (!response.ok) {
    const error = await responseError(
      response,
      `Metric could not load this Replay segment (HTTP ${response.status}).`,
    );
    if (invalidatesSession(error)) invalidateSession();
    throw error;
  }
  return response.arrayBuffer();
}

async function blobRequest(path: string): Promise<Blob> {
  const headers = authenticatedHeaders();
  headers.set('accept', 'application/octet-stream');
  let response: Response;
  try {
    response = await fetch(path, { headers, credentials: 'include' });
  } catch {
    throw new ApiError(
      0,
      'network_error',
      null,
      'Cannot reach Metric. Check the connection and server status.',
      true,
    );
  }
  if (!response.ok) {
    const error = await responseError(
      response,
      `Metric could not download this attachment (HTTP ${response.status}).`,
    );
    if (invalidatesSession(error)) invalidateSession();
    throw error;
  }
  return response.blob();
}

async function queryDownloadRequest(
  projectId: string,
  body: UnifiedQueryRequest,
  format: 'json' | 'csv',
): Promise<{ blob: Blob; filename: string }> {
  const session = sessionProvider();
  if (!session.csrfToken) {
    invalidateSession();
    throw new ApiError(
      403,
      'csrf_missing',
      null,
      'This tab cannot safely change data. Sign in again to restore the security token.',
    );
  }
  const headers = authenticatedHeaders();
  headers.set('accept', format === 'csv' ? 'text/csv' : 'application/json');
  headers.set('content-type', 'application/json');
  headers.set('x-csrf-token', session.csrfToken);
  let response: Response;
  try {
    response = await fetch(`/api/v1/projects/${projectId}/query`, {
      method: 'POST',
      headers,
      credentials: 'include',
      body: JSON.stringify({
        ...body,
        query: queryForBackend(body.query),
        cursor: undefined,
        output: { kind: 'download', format },
      }),
    });
  } catch {
    throw new ApiError(
      0,
      'network_error',
      null,
      'Cannot reach Metric. Check the connection and server status.',
      true,
    );
  }
  if (!response.ok) {
    const error = await responseError(response);
    if (invalidatesSession(error)) invalidateSession();
    throw error;
  }
  const disposition = response.headers.get('content-disposition') ?? '';
  const filename = disposition.match(/filename="([^"]+)"/)?.[1] ?? `metric-export.${format}`;
  return { blob: await response.blob(), filename };
}

function authenticatedHeaders(): Headers {
  const headers = new Headers();
  const session = sessionProvider();
  if (session.organizationId) headers.set('x-metric-organization-id', session.organizationId);
  return headers;
}

async function responseError(response: Response, fallbackMessage?: string): Promise<ApiError> {
  let body: ApiErrorBody | null = null;
  try {
    body = (await response.json()) as ApiErrorBody;
  } catch {
    // A proxy or binary endpoint may return a non-JSON failure.
  }
  const code = body?.error?.code ?? `http_${response.status}`;
  const message =
    messages[code] ??
    body?.error?.message ??
    fallbackMessage ??
    `Metric returned HTTP ${response.status} without a recognized error.`;
  return new ApiError(
    response.status,
    code,
    body?.error?.request_id ?? response.headers.get('x-request-id'),
    message,
    response.status === 429 || response.status >= 500,
  );
}

function invalidatesSession(error: ApiError): boolean {
  return (
    error.status === 401 ||
    error.code === 'invalid_credentials' ||
    error.code === 'csrf_failed' ||
    error.code === 'csrf_missing'
  );
}

export const api = {
  bootstrap(body: Record<string, unknown>) {
    return request<Identity>(
      '/api/v1/auth/bootstrap',
      { method: 'POST', body: JSON.stringify(body) },
      { public: true },
    );
  },
  login(email: string, password: string, organizationId?: string) {
    return request<LoginResponse>(
      '/api/v1/auth/login',
      {
        method: 'POST',
        body: JSON.stringify({
          email,
          password,
          ...(organizationId ? { organization_id: organizationId } : {}),
        }),
      },
      { public: true },
    );
  },
  setupPassword: (setupToken: string, password: string, organizationId?: string) =>
    request<void>(
      '/api/v1/auth/setup-password',
      {
        method: 'POST',
        body: JSON.stringify({
          setup_token: setupToken,
          password,
          ...(organizationId ? { organization_id: organizationId } : {}),
        }),
      },
      { public: true },
    ),
  me: () => request<Identity>('/api/v1/auth/me'),
  organizations: () => request<{ items: UserOrganization[] }>('/api/v1/auth/organizations'),
  createOrganization: (displayName: string, slug: string) =>
    request<Organization>('/api/v1/organizations', {
      method: 'POST',
      body: JSON.stringify({ display_name: displayName, slug }),
    }),
  logout: () => request<void>('/api/v1/auth/logout', { method: 'POST' }),
  tokens: () => request<{ items: ApiToken[] }>('/api/v1/auth/tokens'),
  createToken: (name: string, scopes: string[], expiresAt: string) =>
    request<CreatedApiToken>('/api/v1/auth/tokens', {
      method: 'POST',
      body: JSON.stringify({ name, scopes, expires_at: expiresAt }),
    }),
  revokeToken: (tokenId: string) =>
    request<void>(`/api/v1/auth/tokens/${tokenId}`, { method: 'DELETE' }),
  organization: () => request<Organization>('/api/v1/organization'),
  organizationMembers: () =>
    request<{ items: OrganizationMember[] }>('/api/v1/organization/members'),
  inviteOrganizationMember: (email: string, displayName: string, role: OrganizationRole) =>
    request<CreatedInvitation>('/api/v1/organization/members', {
      method: 'POST',
      body: JSON.stringify({ email, display_name: displayName, role }),
    }),
  updateOrganizationMember: (
    userId: string,
    action: 'change_role' | 'disable' | 'enable' | 'remove',
    role?: OrganizationRole,
  ) =>
    request<void>(`/api/v1/organization/members/${userId}`, {
      method: 'PATCH',
      body: JSON.stringify({ action, ...(role ? { role } : {}) }),
    }),
  organizationAudit: () =>
    request<{ items: OrganizationAuditRecord[] }>('/api/v1/organization/audit'),
  projects: (organizationId?: string) =>
    request<{ items: Project[] }>('/api/v1/projects', {}, { organizationId }),
  createProject: (project: CreateProjectInput) =>
    request<CreateProjectResponse>('/api/v1/projects', {
      method: 'POST',
      body: JSON.stringify(project),
    }),
  project: (projectId: string) => request<Project>(`/api/v1/projects/${projectId}`),
  keys: (projectId: string) =>
    request<{ items: ProjectKey[] }>(`/api/v1/projects/${projectId}/keys`),
  createKey: (projectId: string, label: string) =>
    request<{ dsn_key: string }>(`/api/v1/projects/${projectId}/keys`, {
      method: 'POST',
      body: JSON.stringify({ label }),
    }),
  disableKey: (projectId: string, key: string) =>
    request<void>(`/api/v1/projects/${projectId}/keys/${key}`, { method: 'DELETE' }),
  requestProjectDeletion: (projectId: string, confirmSlug: string, operationId: string) =>
    request<ProjectDeletionStatus>(`/api/v1/projects/${projectId}`, {
      method: 'DELETE',
      headers: { 'idempotency-key': operationId },
      body: JSON.stringify({ confirm_slug: confirmSlug }),
    }),
  projectDeletionStatus: (projectId: string) =>
    request<ProjectDeletionStatus>(`/api/v1/projects/${projectId}/deletion`),
  cancelProjectDeletion: (projectId: string, operationId: string) =>
    request<ProjectDeletionStatus>(`/api/v1/projects/${projectId}/deletion/cancel`, {
      method: 'POST',
      body: JSON.stringify({ operation_id: operationId }),
    }),
  updatePolicy: (projectId: string, policy: ProjectPolicy) =>
    request<ProjectPolicy>(`/api/v1/projects/${projectId}/policy`, {
      method: 'PATCH',
      body: JSON.stringify({
        expected_revision: policy.revision,
        ip_policy: policy.ip_policy,
        error_enabled: policy.items.error,
        client_report_enabled: policy.items.client_report,
        log_enabled: policy.items.log,
        transaction_enabled: policy.items.transaction,
        span_enabled: policy.items.span,
        feedback_enabled: policy.items.feedback,
        check_in_enabled: policy.items.check_in,
        metric_enabled: policy.items.metric,
        replay_enabled: policy.items.replay,
        max_event_bytes: policy.limits.max_event_bytes,
        max_events_per_second: policy.limits.max_events_per_second,
        burst: policy.limits.burst,
        inbound_filters: policy.inbound_filters,
      }),
    }),
  query: <T = Record<string, string | number | boolean | null>>(
    projectId: string,
    body: UnifiedQueryRequest,
    signal?: AbortSignal,
  ) =>
    request<UnifiedQueryResult<T>>(`/api/v1/projects/${projectId}/query`, {
      method: 'POST',
      signal,
      body: JSON.stringify({
        ...body,
        query: queryForBackend(body.query),
        limit: body.limit ?? (body.result.kind === 'values' ? 20 : 50),
      }),
    }),
  exportQuery: (projectId: string, body: UnifiedQueryRequest, format: 'json' | 'csv') =>
    queryDownloadRequest(projectId, body, format),
  issue: (projectId: string, issueId: string) =>
    request<Issue>(`/api/v1/projects/${projectId}/issues/${issueId}`),
  issueStatistics: (projectId: string, issueId: string) =>
    request<{ items: IssueStatistic[] }>(
      `/api/v1/projects/${projectId}/issues/${issueId}/statistics?limit=100`,
    ),
  issueActivity: (projectId: string, issueId: string) =>
    request<Page<IssueActivity>>(
      `/api/v1/projects/${projectId}/issues/${issueId}/activity?limit=50`,
    ),
  issueEvents: (projectId: string, issueId: string) =>
    request<Page<Event>>(`/api/v1/projects/${projectId}/issues/${issueId}/events?limit=25`),
  mutateIssue: (projectId: string, issueId: string, action: string) =>
    request<{ applied: boolean; issue: Issue }>(
      `/api/v1/projects/${projectId}/issues/${issueId}/lifecycle`,
      {
        method: 'POST',
        body: JSON.stringify({
          action,
          idempotency_key: randomHexId(),
        }),
      },
    ),
  event: (projectId: string, eventId: string) =>
    request<Event>(`/api/v1/projects/${projectId}/events/${eventId}`),
  monitors: (projectId: string) =>
    request<{ items: CronMonitor[] }>(`/api/v1/projects/${projectId}/monitors?limit=100000`),
  putMonitor: (projectId: string, monitor: MonitorInput) =>
    request<CronMonitor>(`/api/v1/projects/${projectId}/monitors`, {
      method: 'POST',
      body: JSON.stringify(monitor),
    }),
  deleteMonitor: (projectId: string, monitorId: string) =>
    request<void>(`/api/v1/projects/${projectId}/monitors/${monitorId}`, {
      method: 'DELETE',
    }),
  monitorRuns: (
    projectId: string,
    monitorId: string,
    range: { from?: number; until?: number } = {},
    page: { cursor?: string | null; limit?: number } = {},
  ) =>
    request<Page<MonitorRun>>(
      `/api/v1/projects/${projectId}/monitors/${monitorId}/runs${query({
        from: queryTimestamp(range.from),
        until: queryTimestamp(range.until),
        cursor: page.cursor,
        limit: page.limit ?? 100000,
      })}`,
    ),
  log: (projectId: string, logId: string) =>
    request<StructuredLog>(`/api/v1/projects/${projectId}/logs/${logId}`),
  trace: (projectId: string, traceId: string) =>
    request<Trace>(`/api/v1/projects/${projectId}/traces/${traceId}`),
  performance: (
    projectId: string,
    filters: {
      environment?: string;
      release?: string;
      service?: string;
      from?: number;
      until?: number;
    } = {},
  ) =>
    request<{ items: PerformanceBucket[] }>(
      `/api/v1/projects/${projectId}/performance${query({
        environment: filters.environment,
        release: filters.release,
        service: filters.service,
        from: queryTimestamp(filters.from),
        until: queryTimestamp(filters.until),
        limit: 100,
      })}`,
    ),
  replay: (projectId: string, replayId: string) =>
    request<Replay>(`/api/v1/projects/${projectId}/replays/${replayId}`),
  replaySegment: (projectId: string, replayId: string, segmentId: number) =>
    binaryRequest(`/api/v1/projects/${projectId}/replays/${replayId}/segments/${segmentId}`),
  savedQueries: (projectId: string) =>
    request<{ items: SavedQuery[] }>(`/api/v1/projects/${projectId}/saved-queries`),
  createSavedQuery: (projectId: string, name: string, query: ExploreRequest) =>
    request<SavedQuery>(`/api/v1/projects/${projectId}/saved-queries`, {
      method: 'POST',
      body: JSON.stringify({ name, query: exploreRequestForBackend(query) }),
    }),
  updateSavedQuery: (projectId: string, value: SavedQuery) =>
    request<SavedQuery>(`/api/v1/projects/${projectId}/saved-queries/${value.id}`, {
      method: 'PATCH',
      body: JSON.stringify({
        revision: value.revision,
        name: value.name,
        query: exploreRequestForBackend(value.query),
      }),
    }),
  deleteSavedQuery: (projectId: string, id: string) =>
    request<void>(`/api/v1/projects/${projectId}/saved-queries/${id}`, {
      method: 'DELETE',
    }),
  dashboards: (projectId: string) =>
    request<{ items: Dashboard[] }>(`/api/v1/projects/${projectId}/dashboards`),
  createDashboard: (
    projectId: string,
    input: {
      name: string;
      widgets: Array<{ title: string; saved_query_id: string; shape: string }>;
      refresh_interval: string;
    },
  ) =>
    request<Dashboard>(`/api/v1/projects/${projectId}/dashboards`, {
      method: 'POST',
      body: JSON.stringify(input),
    }),
  updateDashboard: (projectId: string, value: Dashboard) =>
    request<Dashboard>(`/api/v1/projects/${projectId}/dashboards/${value.id}`, {
      method: 'PATCH',
      body: JSON.stringify({
        revision: value.revision,
        name: value.name,
        widgets: value.widgets,
        refresh_interval: value.refresh_interval,
      }),
    }),
  deleteDashboard: (projectId: string, id: string) =>
    request<void>(`/api/v1/projects/${projectId}/dashboards/${id}`, {
      method: 'DELETE',
    }),
  refreshDashboard: (
    projectId: string,
    id: string,
    variables: { environment?: string; release?: string },
  ) =>
    request<DashboardRefresh>(`/api/v1/projects/${projectId}/dashboards/${id}/refresh`, {
      method: 'POST',
      body: JSON.stringify(variables),
    }),
  notificationDestinations: (projectId: string) =>
    request<{ items: NotificationDestination[] }>(
      `/api/v1/projects/${projectId}/notification-destinations`,
    ),
  putNotificationDestination: (projectId: string, input: Record<string, unknown>) =>
    request<NotificationDestination>(`/api/v1/projects/${projectId}/notification-destinations`, {
      method: 'POST',
      body: JSON.stringify(input),
    }),
  checkTelegramBot: (projectId: string, token: string) =>
    request<TelegramBot>(`/api/v1/projects/${projectId}/notification-destinations/telegram/check`, {
      method: 'POST',
      body: JSON.stringify({ token }),
    }),
  syncTelegramSubscribers: (projectId: string, token: string, pairingCode: string) =>
    request<TelegramSubscriberSync>(
      `/api/v1/projects/${projectId}/notification-destinations/telegram/sync`,
      {
        method: 'POST',
        body: JSON.stringify({ token, pairing_code: pairingCode }),
      },
    ),
  testNotificationDestination: (projectId: string, destinationId: string) =>
    request<NotificationDelivery>(
      `/api/v1/projects/${projectId}/notification-destinations/${destinationId}/test`,
      { method: 'POST' },
    ),
  notificationDeliveries: (projectId: string) =>
    request<{ items: NotificationDelivery[] }>(
      `/api/v1/projects/${projectId}/notification-deliveries`,
    ),
  alertRules: (projectId: string) =>
    request<{ items: AlertRule[] }>(`/api/v1/projects/${projectId}/alert-rules`),
  putAlertRule: (projectId: string, input: Record<string, unknown>) =>
    request<AlertRule>(`/api/v1/projects/${projectId}/alert-rules`, {
      method: 'POST',
      body: JSON.stringify(input),
    }),
  feedbackItem: (projectId: string, feedbackId: string) =>
    request<Feedback>(`/api/v1/projects/${projectId}/feedback/${feedbackId}`),
  updateFeedbackStatus: (projectId: string, feedbackId: string, status: FeedbackStatus) =>
    request<Feedback>(`/api/v1/projects/${projectId}/feedback/${feedbackId}`, {
      method: 'PATCH',
      body: JSON.stringify({ status }),
    }),
  feedbackAttachment: (projectId: string, feedbackId: string, attachmentId: string) =>
    blobRequest(`/api/v1/projects/${projectId}/feedback/${feedbackId}/attachments/${attachmentId}`),
  release: (projectId: string, releaseId: string) =>
    request<Release>(`/api/v1/projects/${projectId}/releases/${releaseId}`),
  createRelease: (projectId: string, version: string, url?: string) =>
    request<Release>(`/api/v1/projects/${projectId}/releases`, {
      method: 'POST',
      body: JSON.stringify({ version, url: url || null }),
    }),
  finalizeRelease: (projectId: string, releaseId: string) =>
    request<Release>(`/api/v1/projects/${projectId}/releases/${releaseId}/finalize`, {
      method: 'POST',
      body: JSON.stringify({}),
    }),
  releaseDeploys: (projectId: string, releaseId: string) =>
    request<Page<Deploy>>(`/api/v1/projects/${projectId}/releases/${releaseId}/deploys?limit=50`),
  createDeploy: (
    projectId: string,
    releaseId: string,
    input: { environment: string; name?: string; url?: string },
  ) =>
    request<Deploy>(`/api/v1/projects/${projectId}/releases/${releaseId}/deploys`, {
      method: 'POST',
      headers: { 'idempotency-key': randomHexId() },
      body: JSON.stringify(input),
    }),
  releaseIssues: (projectId: string, releaseId: string, kind: 'new' | 'regressed') =>
    request<Page<ReleaseIssue>>(
      `/api/v1/projects/${projectId}/releases/${releaseId}/issues${query({
        kind,
        limit: 20,
      })}`,
    ),
  releaseHealth: (projectId: string, releaseId: string) =>
    request<ReleaseHealth>(`/api/v1/projects/${projectId}/releases/${releaseId}/health`),
  capabilities: () => request<CapabilityDocument>('/api/v1/capabilities', {}, { public: true }),
  status: () => request<ComponentStatus>('/api/v1/status'),
};

export function retryQuery(attempt: number, error: unknown): boolean {
  return error instanceof ApiError && error.retryable && attempt < 1;
}
