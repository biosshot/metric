import * as Sentry from '@sentry/node';
import { argv, exit, stderr, stdout } from 'node:process';

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Metric DSN argument is required');
}

const hardDeadline = setTimeout(() => {
  stderr.write('real Node SDK attachment sender exceeded its 15 second process deadline\n');
  exit(2);
}, 15_000);

(async function main() {
  try {
    Sentry.init({
      dsn,
      tracesSampleRate: 0,
      environment: 'sdk-compatibility',
      release: 'metric-node-sdk-test@1.0.0',
      sendDefaultPii: false,
    });
    Sentry.setTag('metric.sdk_test', 'node-attachment');

    const error = new Error('Metric real Node SDK attachment compatibility event');
    error.name = 'MetricSdkAttachmentCompatibilityError';
    const eventId = Sentry.captureException(error, {
      attachments: [
        {
          filename: 'metric-context.json',
          data: JSON.stringify({ source: 'node-sdk', safe: true }),
          contentType: 'application/json',
        },
      ],
    });
    const flushed = await Sentry.flush(8_000);
    await Sentry.close(2_000);
    if (!flushed) {
      throw new Error('the real Node SDK did not flush the captured attachment Event');
    }

    stdout.write(`${JSON.stringify({ event_id: eventId, flushed })}\n`);
  } finally {
    clearTimeout(hardDeadline);
  }
})();
