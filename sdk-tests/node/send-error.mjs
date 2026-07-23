import * as Sentry from '@sentry/node';
import { argv, exit, stderr, stdout } from 'node:process';

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Faultkeep DSN argument is required');
}

const hardDeadline = setTimeout(() => {
  stderr.write('real Node SDK sender exceeded its 15 second process deadline\n');
  exit(2);
}, 15_000);

(async function main() {
  try {
    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: 'sdk-compatibility',
      release: 'faultkeep-node-sdk-test@1.0.0',
      sendDefaultPii: false,
    });
    Sentry.setTag('faultkeep.sdk_test', 'node');

    const error = new Error('Faultkeep real Node SDK compatibility event');
    error.name = 'FaultkeepSdkCompatibilityError';
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
