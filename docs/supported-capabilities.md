# Supported features

## Available by default

- receive errors from official Sentry SDKs;
- remove or pseudonymize private data before storage;
- group repeated errors into issues;
- search and inspect events in the web interface;
- receive structured logs, transactions and spans;
- track releases, deploys and release health;
- collect user feedback;
- create dashboards and saved searches;
- send email and Telegram alerts;
- monitor cron jobs and HTTP endpoints;
- collect application counters, gauges and distributions;
- manage users, projects, roles, sessions and API tokens;
- set retention periods and project ingest limits;
- export an Incident Capsule for an issue;
- deliver signed webhooks.

## Optional

- local file storage or S3-compatible storage;
- minidump upload for native crashes;
- debug files and JavaScript source maps;
- an external Symbolicator service;
- cold archive for older data;
- browser Session Replay with `@sentry/browser` 10.66.0.

Minidumps and Session Replay are disabled by default because they need an explicit
privacy decision.

## Not available yet

- profiling;
- single sign-on, SCIM, MFA and passkeys;
- multiple Metric processing nodes;
- built-in high-availability deployment;
- automatic migration between different database schema generations;
- search or restore directly from cold archives;
- a Prometheus metrics endpoint.

Metric also provides a machine-readable feature list at
`GET /api/v1/capabilities`.
