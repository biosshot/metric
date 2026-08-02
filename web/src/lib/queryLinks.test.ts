import { describe, expect, it } from 'vitest';
import { queryExpression, queryLink, queryLinks, quoteQueryValue } from './queryLinks';

describe('queryLinks', () => {
  it('quotes values and escapes quotes and backslashes', () => {
    expect(quoteQueryValue('prod\\blue "canary"')).toBe('"prod\\\\blue \\"canary\\""');
  });

  it('builds route objects with the Unified Query v2 q parameter', () => {
    expect(queryLink('/releases', 'rel', 'backend@1.2')).toEqual({
      path: '/releases',
      query: { q: 'rel:"backend@1.2"' },
    });
  });

  it('joins exact predicates without manual query-string construction', () => {
    expect(
      queryLinks('/logs', [
        ['env', 'production'],
        ['trace', '41'.repeat(16)],
      ]),
    ).toEqual({
      path: '/logs',
      query: { q: `env:"production" AND trace:"${'41'.repeat(16)}"` },
    });
    expect(queryExpression([['svc', 'payments']])).toBe('svc:"payments"');
  });
});
