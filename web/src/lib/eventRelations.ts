export interface EventRelations {
  traceId?: string;
  replayId?: string;
  release?: string;
  environment?: string;
  userId?: string;
}

const EXACT_EVENT_ID = /^[0-9a-f]{32}$/i;

function record(value: unknown): Record<string, unknown> | undefined {
  return value !== null && typeof value === 'object' && !Array.isArray(value)
    ? (value as Record<string, unknown>)
    : undefined;
}

function text(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  const trimmed = value.trim();
  return trimmed || undefined;
}

function exactId(value: unknown): string | undefined {
  const candidate = text(value);
  return candidate && EXACT_EVENT_ID.test(candidate) ? candidate : undefined;
}

export function extractEventRelations(body: unknown): EventRelations {
  const event = record(body);
  if (!event) return {};
  const contexts = record(event.contexts);
  const trace = record(contexts?.trace);
  const replay = record(contexts?.replay);
  const user = record(event.user);

  const relations: EventRelations = {};
  const traceId = exactId(trace?.trace_id);
  const replayId = exactId(replay?.replay_id) ?? exactId(event.replay_id);
  const release = text(event.release);
  const environment = text(event.environment);
  const userId = text(user?.id);
  if (traceId) relations.traceId = traceId;
  if (replayId) relations.replayId = replayId;
  if (release) relations.release = release;
  if (environment) relations.environment = environment;
  if (userId) relations.userId = userId;
  return relations;
}
