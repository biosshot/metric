<?php
declare(strict_types=1);

require __DIR__ . '/.deps/vendor/autoload.php';
final class FixtureLogger extends \Psr\Log\AbstractLogger
{
    public function log($level, \Stringable|string $message, array $context = []): void
    {
        if (in_array($level, ['error', 'warning', 'critical'], true)) {
            fwrite(STDERR, preg_replace('~https?://[^\s]+~', '[url]', (string) $message) . "\n");
        }
    }
}
if ($argc !== 2) {
    fwrite(STDERR, "Metric DSN argument is required\n");
    exit(2);
}
\Sentry\init([
    'dsn' => $argv[1],
    'environment' => 'sdk-compatibility',
    'release' => 'metric-php-sdk-test@1.0.0',
    'send_default_pii' => false,
    'traces_sample_rate' => 0.0,
    'http_connect_timeout' => 2,
    'http_timeout' => 5,
    'logger' => new FixtureLogger(),
]);
\Sentry\configureScope(static function (\Sentry\State\Scope $scope): void {
    $scope->setTag('metric.sdk_test', 'php');
});
$eventId = \Sentry\captureException(new \RuntimeException('Metric real PHP SDK compatibility event'));
$client = \Sentry\SentrySdk::getCurrentHub()->getClient();
if ($eventId === null || $client === null) {
    fwrite(STDERR, "PHP SDK did not capture the exception\n");
    exit(2);
}
$flushed = $client->flush(8)->getStatus() === \Sentry\Transport\ResultStatus::success();
if (!$flushed) {
    fwrite(STDERR, "PHP SDK did not flush\n");
    exit(2);
}
echo json_encode(['event_id' => (string) $eventId, 'flushed' => true,
    'runtime' => PHP_VERSION, 'sdk_version' => \Composer\InstalledVersions::getPrettyVersion('sentry/sentry')], JSON_THROW_ON_ERROR), "\n";
