# SDK compatibility

Metric works with official Sentry SDKs. The versions below are tested by running
the real SDK and sending an error event to Metric. This does not mean that every
optional SDK feature is tested on every platform.

| Platform | Tested version |
| --- | --- |
| JavaScript in a browser | `@sentry/browser` 10.66.0 |
| Node.js | `@sentry/node` 10.66.0 |
| Python | `sentry-sdk` 2.32.0 |
| Java | `sentry-java` 8.50.1 |
| .NET | `Sentry` 6.7.0 |
| Go | `sentry-go` 0.48.0 |
| Rust | `sentry` 0.48.5 |
| Sentry CLI | 3.6.2 and 2.58.6 |

A version not listed here may work, but it has not passed the release tests.

## PHP integration notes

The PHP example in SDK setup uses `sentry/sentry` 4.19.1. Error events with an
Event ID only in the envelope or item header are accepted and normalized into the
stored event body. Conflicting IDs and malformed body IDs are still rejected.

A local Docker integration was manually verified with PHP 8.4, Laravel 11,
`sentry/sentry` 4.19.1 and `sentry/sentry-laravel` 4.20.1. The checks exercised
error events, HTTP and SQL spans, structured logs, application metrics and cron
check-ins. These checks are not yet part of the automated release-test matrix
and do not establish compatibility for every optional PHP SDK feature.

Indexed transaction and span names are limited to 1,024 UTF-8 bytes. Longer names
are truncated at a character boundary, while the full name remains in the stored,
scrubbed JSON body. Control characters still cause the span to be rejected.

## Other supported data

Metric also accepts:

- errors and messages;
- structured logs;
- transactions and spans;
- release sessions;
- user feedback;
- cron check-ins;
- application metrics;
- browser Session Replay;
- safe attachments;
- debug files and JavaScript source-map bundles.

Some features depend on the SDK. Session Replay is tested with the listed browser
SDK, attachments are tested with the listed Node.js SDK, and debug-file uploads
are tested with the listed Sentry CLI versions. Other SDK and data-type
combinations may work but are not all part of the release tests.

Session Replay is disabled for each new project until you enable it.

## Not currently covered by release tests

The following SDK families are not part of the automated compatibility matrix:

- Apple and Cocoa;
- Flutter and Dart;
- Android and Kotlin;
- native C++;
- PHP;
- React Native;
- Ruby.

Profiling and legacy StatsD metric items are not supported.

The exact test inventory is stored in
[`compatibility/sentry-sdk-matrix.toml`](https://github.com/biosshot/metric/blob/main/compatibility/sentry-sdk-matrix.toml).
