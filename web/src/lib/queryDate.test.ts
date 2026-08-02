import { describe, expect, it } from 'vitest';
import { queryForBackend } from './queryDate';

describe('query date adapter', () => {
  it('converts a local minute value to a millisecond timestamp', () => {
    const expected = new Date(2026, 7, 2, 9, 20, 0, 0).getTime();
    expect(queryForBackend('date:2026-08-02T09:20')).toBe(`timestamp:${expected}`);
  });

  it('preserves boolean structure, negation, quotes, and comparison operators', () => {
    const from = new Date(2026, 7, 2, 9, 20, 0, 0).getTime();
    const until = new Date(2026, 7, 3, 10, 30, 0, 0).getTime();
    expect(
      queryForBackend('date:>=2026-08-02T09:20 AND (!date:<"2026-08-03T10:30" OR level:error)'),
    ).toBe(`timestamp:>=${from} AND (!timestamp:<${until} OR level:error)`);
  });

  it('does not rewrite invalid dates or date text inside another value', () => {
    expect(queryForBackend('date:2026-02-30T09:20 msg:"on date:2026-08-02T09:20 failure"')).toBe(
      'date:2026-02-30T09:20 msg:"on date:2026-08-02T09:20 failure"',
    );
  });
});
