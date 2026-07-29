const RANGE_MILLIS: Record<string, number> = {
  '1h': 60 * 60 * 1_000,
  '24h': 24 * 60 * 60 * 1_000,
  '7d': 7 * 24 * 60 * 60 * 1_000,
  '30d': 30 * 24 * 60 * 60 * 1_000,
};

export interface TimeWindow {
  from: number;
  until: number;
}

export const MAX_TIME_RANGE_MILLIS = 30 * 24 * 60 * 60 * 1_000;

export function timeWindow(range: string): TimeWindow {
  const until = Date.now();
  if (range === 'all') return { from: 0, until };
  return { from: until - (RANGE_MILLIS[range] ?? RANGE_MILLIS['24h']), until };
}

export function optionalTimeWindow(
  range: string,
  window: TimeWindow,
): { from?: number; until?: number } {
  return range === 'all' ? {} : window;
}

export function localDateTime(value: number): string {
  const date = new Date(value);
  return new Date(value - date.getTimezoneOffset() * 60_000).toISOString().slice(0, 16);
}

export function parseCustomTimeWindow(from: string, until: string): TimeWindow | null {
  const parsedFrom = new Date(from).getTime();
  const parsedUntil = new Date(until).getTime();
  if (
    !Number.isFinite(parsedFrom) ||
    !Number.isFinite(parsedUntil) ||
    parsedFrom >= parsedUntil ||
    parsedUntil - parsedFrom > MAX_TIME_RANGE_MILLIS
  ) {
    return null;
  }
  return { from: parsedFrom, until: parsedUntil };
}
