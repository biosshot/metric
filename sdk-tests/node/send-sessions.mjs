import * as Sentry from '@sentry/node';
import { argv, exit, stderr, stdout } from 'node:process';

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Metric DSN argument is required');
}

const hardDeadline = setTimeout(() => {
  stderr.write('real Node SDK Session sender exceeded its 15 second process deadline\n');
  exit(2);
}, 15_000);

try {
  Sentry.init({
    dsn,
    tracesSampleRate: 0,
    environment: 'sdk-compatibility',
    release: 'metric-node-sessions@1.0.0',
    sendDefaultPii: false,
  });
  Sentry.setUser({ id: 'phase-30-user' });

  const healthy = Sentry.startSession();
  Sentry.captureSession();
  Sentry.captureSession(true);

  const crashed = Sentry.startSession();
  crashed.status = 'crashed';
  crashed.errors = 1;
  Sentry.captureSession();

  const flushed = await Sentry.flush(8_000);
  await Sentry.close(2_000);
  if (!flushed) {
    throw new Error('the real Node SDK did not flush Session lifecycle updates');
  }

  stdout.write(
    `${JSON.stringify({
      healthy_session_id: healthy.sid,
      crashed_session_id: crashed.sid,
      flushed,
    })}\n`,
  );
} finally {
  clearTimeout(hardDeadline);
}
