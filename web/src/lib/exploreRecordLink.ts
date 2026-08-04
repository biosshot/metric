import type { RouteLocationRaw } from 'vue-router';
import type { ExploreScalar, QuerySource } from '../api/types';

type ExploreRecord = Record<string, ExploreScalar>;

const HEX_ID = /^[0-9a-f]{32}$/i;

function identifier(record: ExploreRecord, field: string): string | null {
  const value = record[field];
  return typeof value === 'string' && HEX_ID.test(value) ? value.toLowerCase() : null;
}

export function exploreRecordLink(
  source: QuerySource,
  record: ExploreRecord,
): RouteLocationRaw | null {
  if (source === 'errors') {
    const eventId = identifier(record, 'event_id');
    return eventId ? { path: `/events/${eventId}` } : null;
  }
  if (source === 'logs') {
    const logId = identifier(record, 'id');
    return logId ? { path: `/logs/${logId}` } : null;
  }
  if (source === 'traces') {
    const traceId = identifier(record, 'trace_id');
    return traceId ? { path: `/traces/${traceId}` } : null;
  }
  return null;
}
