import type {
  ApiErrorBody,
  ApiToken,
  CapabilityDocument,
  ComponentStatus,
  CreatedInvitation,
  CreatedApiToken,
  CreateProjectInput,
  CreateProjectResponse,
  Event,
  Feedback,
  FeedbackStatus,
  Identity,
  Issue,
  IssueActivity,
  IssueStatistic,
  LoginResponse,
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
  ReleaseSummary,
  Deploy,
  Span,
  StructuredLog,
  Trace,
} from './types';

type SessionProvider = () => { organizationId: string | null; csrfToken: string | null };

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
  search_syntax_invalid: 'The search expression is not valid.',
  search_field_not_indexed: 'That search field is not indexed.',
  search_limit_exceeded: 'The search is too complex. Remove some conditions.',
  search_requires_positive_anchor: 'Add at least one positive indexed search condition.',
  search_too_broad: 'The search is too broad. Add another condition.',
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

export function configureSession(provider: SessionProvider): void {
  sessionProvider = provider;
}

function isMutation(method: string): boolean {
  return !['GET', 'HEAD', 'OPTIONS'].includes(method);
}

async function request<T>(
  path: string,
  init: RequestInit = {},
  options: { public?: boolean } = {},
): Promise<T> {
  const method = (init.method ?? 'GET').toUpperCase();
  const session = sessionProvider();
  const headers = new Headers(init.headers);
  headers.set('accept', 'application/json');
  if (init.body) headers.set('content-type', 'application/json');
  if (!options.public && session.organizationId) {
    headers.set('x-metric-organization-id', session.organizationId);
  }
  if (!options.public && isMutation(method)) {
    if (!session.csrfToken) {
      throw new ApiError(
        403,
        'csrf_missing',
        null,
        'This tab cannot safely change data. Sign in again to restore the security token.',
      );
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
    let body: ApiErrorBody | null = null;
    try {
      body = (await response.json()) as ApiErrorBody;
    } catch {
      // A proxy may return a non-JSON failure. The status remains visible below.
    }
    const code = body?.error?.code ?? `http_${response.status}`;
    const message =
      messages[code] ??
      body?.error?.message ??
      `Metric returned HTTP ${response.status} without a recognized error.`;
    throw new ApiError(
      response.status,
      code,
      body?.error?.request_id ?? response.headers.get('x-request-id'),
      message,
      response.status === 429 || response.status >= 500,
    );
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

export const api = {
  bootstrap(body: Record<string, unknown>) {
    return request<Identity>(
      '/api/v1/auth/bootstrap',
      { method: 'POST', body: JSON.stringify(body) },
      { public: true },
    );
  },
  login(email: string, password: string, organizationId: string) {
    return request<LoginResponse>(
      '/api/v1/auth/login',
      {
        method: 'POST',
        body: JSON.stringify({ email, password, organization_id: organizationId }),
      },
      { public: true },
    );
  },
  setupPassword: (setupToken: string, password: string, organizationId: string) =>
    request<void>(
      '/api/v1/auth/setup-password',
      {
        method: 'POST',
        body: JSON.stringify({
          setup_token: setupToken,
          password,
          organization_id: organizationId,
        }),
      },
      { public: true },
    ),
  me: () => request<Identity>('/api/v1/auth/me'),
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
  projects: () => request<{ items: Project[] }>('/api/v1/projects'),
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
        max_event_bytes: policy.limits.max_event_bytes,
        max_events_per_second: policy.limits.max_events_per_second,
        burst: policy.limits.burst,
        inbound_filters: policy.inbound_filters,
      }),
    }),
  issues: (projectId: string, status?: string, cursor?: string | null) =>
    request<Page<Issue>>(
      `/api/v1/projects/${projectId}/issues${query({ status, cursor, limit: 50 })}`,
    ),
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
          idempotency_key: crypto.randomUUID().replaceAll('-', ''),
        }),
      },
    ),
  event: (projectId: string, eventId: string) =>
    request<Event>(`/api/v1/projects/${projectId}/events/${eventId}`),
  logs: (
    projectId: string,
    filters: {
      level?: string;
      message?: string;
      environment?: string;
      release?: string;
      service?: string;
      traceId?: string;
      cursor?: string | null;
    } = {},
  ) =>
    request<Page<StructuredLog>>(
      `/api/v1/projects/${projectId}/logs${query({
        level: filters.level,
        message: filters.message,
        environment: filters.environment,
        release: filters.release,
        service: filters.service,
        trace_id: filters.traceId,
        cursor: filters.cursor,
        limit: 50,
      })}`,
    ),
  log: (projectId: string, logId: string) =>
    request<StructuredLog>(`/api/v1/projects/${projectId}/logs/${logId}`),
  transactions: (
    projectId: string,
    filters: {
      environment?: string;
      release?: string;
      service?: string;
      cursor?: string | null;
    } = {},
  ) =>
    request<Page<Span>>(
      `/api/v1/projects/${projectId}/transactions${query({
        environment: filters.environment,
        release: filters.release,
        service: filters.service,
        cursor: filters.cursor,
        limit: 50,
      })}`,
    ),
  trace: (projectId: string, traceId: string) =>
    request<Trace>(`/api/v1/projects/${projectId}/traces/${traceId}`),
  performance: (
    projectId: string,
    filters: { environment?: string; release?: string; service?: string } = {},
  ) =>
    request<{ items: PerformanceBucket[] }>(
      `/api/v1/projects/${projectId}/performance${query({
        environment: filters.environment,
        release: filters.release,
        service: filters.service,
        limit: 100,
      })}`,
    ),
  feedback: (projectId: string, status?: string, cursor?: string | null) =>
    request<Page<Feedback>>(
      `/api/v1/projects/${projectId}/feedback${query({ status, cursor, limit: 50 })}`,
    ),
  feedbackItem: (projectId: string, feedbackId: string) =>
    request<Feedback>(`/api/v1/projects/${projectId}/feedback/${feedbackId}`),
  updateFeedbackStatus: (projectId: string, feedbackId: string, status: FeedbackStatus) =>
    request<Feedback>(`/api/v1/projects/${projectId}/feedback/${feedbackId}`, {
      method: 'PATCH',
      body: JSON.stringify({ status }),
    }),
  feedbackAttachmentUrl: (projectId: string, feedbackId: string, attachmentId: string) =>
    `/api/v1/projects/${projectId}/feedback/${feedbackId}/attachments/${attachmentId}`,
  releases: (projectId: string, cursor?: string | null) =>
    request<Page<ReleaseSummary>>(
      `/api/v1/projects/${projectId}/releases${query({ cursor, limit: 50 })}`,
    ),
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
      headers: { 'idempotency-key': crypto.randomUUID().replaceAll('-', '') },
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
  search: (projectId: string, expression: string, cursor?: string | null) =>
    request<Page<Event> & { candidates_examined: number }>(
      `/api/v1/projects/${projectId}/events/search${query({
        q: expression,
        cursor,
        limit: 50,
      })}`,
    ),
  capabilities: () => request<CapabilityDocument>('/api/v1/capabilities', {}, { public: true }),
  status: () => request<ComponentStatus>('/api/v1/status'),
};

export function retryQuery(attempt: number, error: unknown): boolean {
  return error instanceof ApiError && error.retryable && attempt < 1;
}
