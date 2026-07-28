import * as Sentry from '@sentry/node';
import { argv, exit, stderr, stdout } from 'node:process';

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Faultkeep DSN argument is required');
}

const hardDeadline = setTimeout(() => {
  stderr.write('real Node SDK metric sender exceeded its 15 second process deadline\n');
  exit(2);
}, 15_000);

(async function main() {
  try {
    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: 'sdk-compatibility',
      release: 'faultkeep-node-metrics@1.0.0',
      sendDefaultPii: false,
    });

    Sentry.metrics.count('faultkeep.sdk.requests', 2, {
      unit: 'none',
      attributes: { route: '/checkout', fixture: 'node' },
    });
    Sentry.metrics.gauge('faultkeep.sdk.queue', 7, {
      unit: 'item',
      attributes: { queue: 'payments', fixture: 'node' },
    });
    Sentry.metrics.distribution('faultkeep.sdk.duration', 12.5, {
      unit: 'millisecond',
      attributes: { route: '/checkout', fixture: 'node' },
    });

    const flushed = await Sentry.flush(8_000);
    await Sentry.close(2_000);
    if (!flushed) {
      throw new Error('the real Node SDK did not flush metric fixtures');
    }
    stdout.write(`${JSON.stringify({ flushed, metrics: 3 })}\n`);
  } finally {
    clearTimeout(hardDeadline);
  }
})();
