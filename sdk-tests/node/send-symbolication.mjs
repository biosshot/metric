import * as Sentry from '@sentry/node';
import { argv, stdout } from 'node:process';
import { createRequire } from 'node:module';

const [dsn, bundle] = argv.slice(2);
if (!dsn || !bundle) throw new Error('DSN and compiled fixture are required');
const { metricSourceMapHandle } = createRequire(import.meta.url)(bundle);
Sentry.init({
  dsn,
  environment: 'symbolication-local',
  release: 'symbolication-demo@1',
  tracesSampleRate: 0,
  sendDefaultPii: false,
  autoSessionTracking: false,
});
let eventId;
try {
  metricSourceMapHandle();
} catch (error) {
  eventId = Sentry.captureException(error);
}
const flushed = await Sentry.flush(8000);
await Sentry.close(2000);
if (!flushed || !eventId) throw new Error('Node symbolication fixture did not flush');
stdout.write(`${JSON.stringify({ event_id: eventId, flushed })}\n`);
