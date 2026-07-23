export type Permission =
  | 'event:read'
  | 'issue:read'
  | 'issue:write'
  | 'project:read'
  | 'project:admin'
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
}

export interface ProjectPolicy {
  revision: number;
  ip_policy: 'hmac' | 'keep' | 'remove' | 'truncate';
  items: { error: boolean; client_report: boolean };
  limits: {
    max_event_bytes: number;
    max_events_per_second: number | null;
    burst: number | null;
  };
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

export interface ProjectKey {
  dsn_key: string;
  project_id: string;
  state: 'active' | 'disabled' | 'suspended_by_deletion';
  label: string;
  created_at: string;
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
