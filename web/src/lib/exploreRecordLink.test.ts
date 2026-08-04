import { describe, expect, it } from 'vitest';
import { exploreRecordLink } from './exploreRecordLink';

describe('exploreRecordLink', () => {
  it('opens Error events, Logs and Span traces from table records', () => {
    const eventId = '11'.repeat(16);
    const logId = '22'.repeat(16);
    const traceId = '33'.repeat(16);

    expect(exploreRecordLink('errors', { event_id: eventId })).toEqual({
      path: `/events/${eventId}`,
    });
    expect(exploreRecordLink('logs', { id: logId })).toEqual({ path: `/logs/${logId}` });
    expect(exploreRecordLink('traces', { trace_id: traceId })).toEqual({
      path: `/traces/${traceId}`,
    });
  });

  it('does not link Metrics or records without a valid detail identifier', () => {
    expect(exploreRecordLink('errors', { event_id: null })).toBeNull();
    expect(exploreRecordLink('logs', { id: 'not-an-id' })).toBeNull();
    expect(exploreRecordLink('metrics', { trace_id: null })).toBeNull();
    expect(exploreRecordLink('metrics', { trace_id: '44'.repeat(16) })).toBeNull();
    expect(exploreRecordLink('issues', { id: '44'.repeat(16) })).toBeNull();
  });
});
