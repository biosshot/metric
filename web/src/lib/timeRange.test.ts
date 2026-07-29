import { afterEach, describe, expect, it, vi } from 'vitest';
import { optionalTimeWindow, parseCustomTimeWindow, timeWindow } from './timeRange';

describe('timeWindow', () => {
  afterEach(() => vi.restoreAllMocks());

  it('keeps a stable bounded window for the selected preset', () => {
    vi.spyOn(Date, 'now').mockReturnValue(10_000_000);
    expect(timeWindow('1h')).toEqual({
      from: 10_000_000 - 60 * 60 * 1_000,
      until: 10_000_000,
    });
  });

  it('uses the Unix epoch sentinel for all time and omits list bounds', () => {
    vi.spyOn(Date, 'now').mockReturnValue(10_000_000);
    const window = timeWindow('all');
    expect(window).toEqual({ from: 0, until: 10_000_000 });
    expect(optionalTimeWindow('all', window)).toEqual({});
    expect(optionalTimeWindow('7d', window)).toEqual(window);
  });

  it('accepts custom periods up to 30 days and rejects invalid bounds', () => {
    expect(parseCustomTimeWindow('2026-07-01T10:00', '2026-07-08T10:00')).toEqual({
      from: new Date('2026-07-01T10:00').getTime(),
      until: new Date('2026-07-08T10:00').getTime(),
    });
    expect(parseCustomTimeWindow('2026-07-08T10:00', '2026-07-01T10:00')).toBeNull();
    expect(parseCustomTimeWindow('2026-06-01T10:00', '2026-07-08T10:00')).toBeNull();
  });
});
