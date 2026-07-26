import * as Sentry from '@sentry/node';
import { argv, exit, stderr, stdout } from 'node:process';
import { URL } from 'node:url';

/* global AbortSignal, fetch */

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Metric DSN argument is required');
}

const hardDeadline = setTimeout(() => {
  stderr.write('real Node SDK sender exceeded its 15 second process deadline\n');
  exit(2);
}, 15_000);

async function requireFaultkeepReady(dsn) {
  const readyUrl = new URL(dsn);
  readyUrl.username = '';
  readyUrl.password = '';
  readyUrl.pathname = '/ready';
  readyUrl.search = '';
  readyUrl.hash = '';

  let response;
  try {
    response = await fetch(readyUrl, {
      signal: AbortSignal.timeout(3_000),
    });
  } catch (error) {
    throw new Error(`Faultkeep is not reachable at ${readyUrl.origin}`, {
      cause: error,
    });
  }
  if (!response.ok) {
    throw new Error(`Faultkeep readiness failed with HTTP ${response.status} at ${readyUrl.href}`);
  }
}

(async function main() {
  try {
    await requireFaultkeepReady(dsn);
    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: 'sdk-compatibility',
      release: 'metric-node-sdk-test@1.0.0',
      sendDefaultPii: false,
    });
    Sentry.setTag('metric.sdk_test', 'node');

    const error = new Error('Metric real Node SDK compatibility event');
    error.name = 'MetricSdkCompatibilityError';
    const eventId = Sentry.captureException(error);
    const flushed = await Sentry.flush(8_000);
    await Sentry.close(2_000);
    if (!flushed) {
      throw new Error('the real Node SDK did not flush the captured Event');
    }

    stdout.write(`${JSON.stringify({ event_id: eventId, flushed })}\n`);
  } finally {
    clearTimeout(hardDeadline);
  }
})();
