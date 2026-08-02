import { describe, expect, it } from 'vitest';
import { extractEventRelations } from './eventRelations';

const traceId = '0123456789abcdef0123456789abcdef';
const replayId = 'fedcba9876543210fedcba9876543210';

describe('extractEventRelations', () => {
  it('extracts every exact Event relation', () => {
    expect(
      extractEventRelations({
        contexts: { trace: { trace_id: traceId }, replay: { replay_id: replayId } },
        replay_id: '1'.repeat(32),
        release: 'backend@2.0',
        environment: 'production',
        user: { id: 'user-42', email: 'ignored@example.com', username: 'ignored' },
      }),
    ).toEqual({
      traceId,
      replayId,
      release: 'backend@2.0',
      environment: 'production',
      userId: 'user-42',
    });
  });

  it.each([
    ['traceId', { contexts: { replay: { replay_id: replayId } } }],
    ['replayId', { contexts: { trace: { trace_id: traceId } } }],
    ['release', { environment: 'production' }],
    ['environment', { release: 'backend@2.0' }],
    ['userId', { user: { email: 'ignored@example.com', username: 'ignored' } }],
  ])('omits %s when its exact source value is absent', (key, body) => {
    expect(extractEventRelations(body)).not.toHaveProperty(key);
  });

  it.each([null, [], 'event', 42, { contexts: [] }, { contexts: { trace: [] } }])(
    'returns safe empty relations for malformed body %#',
    (body) => {
      expect(extractEventRelations(body)).toEqual({});
    },
  );

  it('rejects malformed Trace and Replay IDs and uses the exact Replay fallback', () => {
    expect(
      extractEventRelations({
        contexts: {
          trace: { trace_id: 'not-a-trace' },
          replay: { replay_id: 'g'.repeat(32) },
        },
        replay_id: replayId,
      }),
    ).toEqual({ replayId });
  });

  it('does not infer a user relation from email or username', () => {
    expect(
      extractEventRelations({ user: { email: 'a@example.com', username: 'ada' } }).userId,
    ).toBe(undefined);
  });
});
