const RANGE_MILLIS: Record<string, number> = {
  '1h': 60 * 60 * 1_000,
  '24h': 24 * 60 * 60 * 1_000,
  '7d': 7 * 24 * 60 * 60 * 1_000,
  '30d': 30 * 24 * 60 * 60 * 1_000,
};

export function timeWindow(range: string): { from: number; until: number } {
  const until = Date.now();
  return { from: until - (RANGE_MILLIS[range] ?? RANGE_MILLIS['24h']), until };
}
