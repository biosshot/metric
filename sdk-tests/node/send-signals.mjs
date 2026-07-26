import * as Sentry from '@sentry/node';
import { argv, stdout } from 'node:process';

const [dsn] = argv.slice(2);
if (!dsn) {
  throw new Error('Metric DSN argument is required');
}

// Инициализация Sentry
Sentry.init({
  dsn,
  enableLogs: true,
  tracesSampleRate: 1.0,
  environment: 'sdk-compatibility',
  release: 'metric-node-signals@1.1.0',
});

// Главный спан запроса
await Sentry.startSpan(
  {
    name: 'POST /api/v1/process-data',
    op: 'http.server',
    attributes: {
      'service.name': 'compatibility-api',
      'http.method': 'POST',
    },
  },
  async () => {
    Sentry.logger.info('Starting complex scenario validation...');

    // 1. Оставляем хлебный мякиш (breadcrumb) для контекста
    Sentry.addBreadcrumb({
      category: 'auth',
      message: 'Authenticated user token successfully verified',
      level: 'info',
    });

    // 2. Спаны, выполняющиеся параллельно через Promise.all
    await Sentry.startSpan({ name: 'Parallel Operations Task', op: 'queue.process' }, async () => {
      await Promise.all([
        // Параллельный запрос к БД №1
        Sentry.startSpan(
          {
            name: 'SELECT users WHERE id = $1',
            op: 'db.sql.query',
            attributes: { 'db.system': 'postgresql', 'service.name': 'users-db' },
          },
          () => new Promise((resolve) => setTimeout(resolve, 50)),
        ),
        // Параллельный запрос к БД №2 (Redis/Mongo)
        Sentry.startSpan(
          {
            name: 'GET user:session:cache',
            op: 'db.redis',
            attributes: { 'db.system': 'redis', 'service.name': 'cache-redis' },
          },
          () => new Promise((resolve) => setTimeout(resolve, 20)),
        ),
      ]);
    });

    // 3. Спан внешнего HTTP-запроса (Имитация распределенной трассировки)
    await Sentry.startSpan(
      {
        name: 'GET /v2/exchange-rates',
        op: 'http.client',
        attributes: {
          'http.method': 'GET',
          url: 'https://external-finance.com',
        },
      },
      async () => {
        // Генерируем заголовки трассировки для передачи в другой сервис
        const traceHeaders = Sentry.getTraceData();

        Sentry.logger.info('Simulating outgoing HTTP request headers', {
          traceparent: traceHeaders['traceparent'],
          baggage: traceHeaders['baggage'],
        });

        await new Promise((resolve) => setTimeout(resolve, 100));
      },
    );

    // 4. Спан с контролируемой ошибкой внутри (для проверки связывания спана и ошибки)
    try {
      await Sentry.startSpan(
        {
          name: 'TRANSACTION Process payment',
          op: 'internal.payment',
          attributes: { amount: 49.99, currency: 'USD' },
        },
        async () => {
          // Имитируем бизнес-логику, которая упала
          throw new Error('Payment gateway timeout');
        },
      );
    } catch (error) {
      // Логируем ошибку структурировано
      Sentry.logger.error('Payment step failed, capturing exception...', error);
      // Привязываем ошибку к текущему контексту
      Sentry.captureException(error);
    }
  },
);

// Сброс данных в Sentry
const flushed = await Sentry.flush(5_000);
await Sentry.close(5_000);

if (!flushed) {
  throw new Error('Sentry SDK did not flush signals before the deadline');
}

stdout.write('Metric Node complex Traces, Breadcrumbs, and Errors were flushed\n');
