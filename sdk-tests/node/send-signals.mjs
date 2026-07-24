import * as Sentry from '@sentry/node';
import { argv, stdout } from 'node:process';

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Faultkeep DSN argument is required');
}

Sentry.init({
  dsn,
  enableLogs: true,
  tracesSampleRate: 1,
  environment: 'sdk-compatibility',
  release: 'faultkeep-node-signals@1.0.0',
});

await Sentry.startSpan(
  {
    name: 'GET /sdk-compatibility',
    op: 'http.server',
    attributes: { 'service.name': 'compatibility-api' },
  },
  async () => {
    Sentry.logger.info('Faultkeep real Node SDK structured log', {
      'service.name': 'compatibility-api',
      scenario: 'structured-logs',
    });
    await Sentry.startSpan(
      {
        name: 'SELECT compatibility',
        op: 'db.sql.query',
        attributes: {
          'service.name': 'compatibility-database',
          'db.system': 'mongodb',
        },
      },
      async () => Promise.resolve(),
    );
  },
);

const flushed = await Sentry.flush(5_000);
await Sentry.close(5_000);
if (!flushed) {
  throw new Error('Sentry SDK did not flush signals before the deadline');
}

stdout.write('Faultkeep Node structured log and Trace were flushed\n');
