import { afterEach, describe, expect, it, vi } from 'vitest';
import { timeWindow } from './timeRange';

describe('timeWindow', () => {
  afterEach(() => vi.restoreAllMocks());

  it('keeps a stable bounded window for the selected preset', () => {
    vi.spyOn(Date, 'now').mockReturnValue(10_000_000);
    expect(timeWindow('1h')).toEqual({
      from: 10_000_000 - 60 * 60 * 1_000,
      until: 10_000_000,
    });
  });
});
