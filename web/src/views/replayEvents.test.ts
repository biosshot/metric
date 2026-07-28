import { describe, expect, it } from 'vitest';
import { prepareReplayEvents } from './replayEvents';

describe('prepareReplayEvents', () => {
  it('normalizes mixed pinned Sentry custom events from seconds to milliseconds', () => {
    const events = prepareReplayEvents([
      { type: 0, timestamp: 1_785_230_878_929 },
      {
        type: 5,
        timestamp: 1_785_230_878.8832,
        data: { tag: 'performanceSpan' },
      },
      {
        type: 5,
        timestamp: 1_785_230_879.1,
        data: { tag: 'breadcrumb' },
      },
      {
        type: 5,
        timestamp: 1_785_230_879_150,
        data: { tag: 'breadcrumb' },
      },
      { type: 3, timestamp: 1_785_230_879_292 },
    ]);

    expect(events[1]?.timestamp).toBeCloseTo(1_785_230_878_883.2, 1);
    expect(events[2]?.timestamp).toBe(1_785_230_879_100);
    expect(events[3]?.timestamp).toBe(1_785_230_879_150);
    const timestamps = events.map((event) => event.timestamp);
    expect(Math.max(...timestamps) - Math.min(...timestamps)).toBeLessThan(500);
  });

  it('rejects malformed and unbounded timelines before mounting rrweb', () => {
    expect(() => prepareReplayEvents([{ type: 0, timestamp: 0 }])).toThrow(
      'invalid rrweb timestamp',
    );
    expect(() =>
      prepareReplayEvents([
        { type: 0, timestamp: 1_785_230_878_929 },
        { type: 3, timestamp: 1_785_317_278_930 },
      ]),
    ).toThrow('24-hour player limit');
  });
});
