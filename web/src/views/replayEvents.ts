const MAX_REPLAY_TIMELINE_MS = 24 * 60 * 60 * 1000;
const MIN_EPOCH_SECONDS = 1_000_000_000;
const MAX_EPOCH_SECONDS = 10_000_000_000;

type ReplayEvent = Record<string, unknown> & {
  timestamp: number;
};

function isSentryPerformanceSpan(event: Record<string, unknown>): boolean {
  const data = event.data;
  return (
    event.type === 5 &&
    typeof data === 'object' &&
    data !== null &&
    (data as Record<string, unknown>).tag === 'performanceSpan'
  );
}

export function prepareReplayEvents(values: unknown[]): ReplayEvent[] {
  let minimum = Number.POSITIVE_INFINITY;
  let maximum = Number.NEGATIVE_INFINITY;
  const events = values.map((value) => {
    if (typeof value !== 'object' || value === null) {
      throw new Error('Replay contains a malformed rrweb event.');
    }
    const event = value as Record<string, unknown>;
    const rawTimestamp = event.timestamp;
    if (typeof rawTimestamp !== 'number' || !Number.isFinite(rawTimestamp) || rawTimestamp <= 0) {
      throw new Error('Replay contains an invalid rrweb timestamp.');
    }
    const timestamp =
      isSentryPerformanceSpan(event) &&
      rawTimestamp >= MIN_EPOCH_SECONDS &&
      rawTimestamp < MAX_EPOCH_SECONDS
        ? rawTimestamp * 1000
        : rawTimestamp;
    minimum = Math.min(minimum, timestamp);
    maximum = Math.max(maximum, timestamp);
    return timestamp === rawTimestamp
      ? (event as ReplayEvent)
      : ({ ...event, timestamp } as ReplayEvent);
  });
  if (maximum - minimum > MAX_REPLAY_TIMELINE_MS) {
    throw new Error('Replay timeline exceeds the 24-hour player limit.');
  }
  return events;
}
