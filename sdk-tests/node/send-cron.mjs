import * as Sentry from '@sentry/node';
import { argv, exit, stderr, stdout } from 'node:process';
import { URL } from 'node:url';

/* global AbortSignal, fetch */

const [dsn, monitorSlug = 'metric-node-cron'] = argv.slice(2);
if (!dsn) {
  throw new Error('Metric DSN argument is required');
}

const hardDeadline = setTimeout(() => {
  stderr.write('real Node SDK cron sender exceeded its 15 second process deadline\n');
  exit(2);
}, 15_000);

async function requireMetricReady(value) {
  const readyUrl = new URL(value);
  readyUrl.username = '';
  readyUrl.password = '';
  readyUrl.pathname = '/ready';
  const response = await fetch(readyUrl, { signal: AbortSignal.timeout(3_000) });
  if (!response.ok) {
    throw new Error(`Metric readiness failed with HTTP ${response.status}`);
  }
}

(async function main() {
  try {
    await requireMetricReady(dsn);
    Sentry.init({
      dsn,
      environment: 'sdk-compatibility',
      release: 'metric-node-sdk-test@1.0.0',
      tracesSampleRate: 0,
      sendDefaultPii: false,
    });
    const checkInId = Sentry.captureCheckIn(
      { monitorSlug, status: 'in_progress' },
      {
        schedule: { type: 'interval', value: 5, unit: 'minute' },
        checkinMargin: 1,
        maxRuntime: 10,
        timezone: 'UTC',
      },
    );
    Sentry.captureCheckIn({
      monitorSlug,
      checkInId,
      status: 'ok',
      duration: 0.25,
    });
    const errorCheckInId = Sentry.captureCheckIn(
      { monitorSlug: `${monitorSlug}-error`, status: 'error' },
      {
        schedule: { type: 'interval', value: 5, unit: 'minute' },
        checkinMargin: 1,
        maxRuntime: 10,
        timezone: 'UTC',
      },
    );
    const flushed = await Sentry.flush(8_000);
    await Sentry.close(2_000);
    if (!flushed) {
      throw new Error('the real Node SDK did not flush the cron check-ins');
    }
    stdout.write(
      `${JSON.stringify({
        check_in_id: checkInId,
        error_check_in_id: errorCheckInId,
        monitor_slug: monitorSlug,
      })}\n`,
    );
  } finally {
    clearTimeout(hardDeadline);
  }
})();
