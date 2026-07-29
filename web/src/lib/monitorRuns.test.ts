import { describe, expect, it } from 'vitest';
import type { MonitorRun } from '../api/types';
import { newestMonitorRunsFirst, sampleMonitorTimeline } from './monitorRuns';

describe('monitor run presentation', () => {
  it('orders runs newest first with a deterministic id tie-breaker', () => {
    const runs = [run(1, 100), run(3, 300), run(2, 300)];

    expect(newestMonitorRunsFirst(runs).map(({ id }) => id)).toEqual(['3', '2', '1']);
    expect(runs.map(({ id }) => id)).toEqual(['1', '3', '2']);
  });

  it('bounds timeline points while retaining endpoints and sharp extrema', () => {
    const runs = Array.from({ length: 2_000 }, (_, index) =>
      run(index, index, index === 700 ? 100_000 : index === 701 ? 0 : 1_000 + (index % 10)),
    );

    const sampled = sampleMonitorTimeline(runs, 512);

    expect(sampled).toHaveLength(512);
    expect(sampled[0].id).toBe('1999');
    expect(sampled.at(-1)?.id).toBe('0');
    expect(sampled.some(({ id }) => id === '700')).toBe(true);
    expect(sampled.some(({ id }) => id === '701')).toBe(true);
    expect(
      sampled.every(
        (item, index) => index === 0 || item.started_at <= sampled[index - 1].started_at,
      ),
    ).toBe(true);
  });
});

function run(id: number, startedAt: number, durationMs = 100): MonitorRun {
  return {
    id: String(id),
    monitor_id: 'monitor',
    status: 'success',
    source: 'scheduler',
    scheduled_for: null,
    started_at: new Date(startedAt).toISOString(),
    finished_at: null,
    duration_ms: durationMs,
    received_at: new Date(startedAt).toISOString(),
    release_id: null,
    http_status: null,
    uptime_failure: null,
  };
}
