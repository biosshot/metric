# Supported features

## Included

- receive errors from official Sentry SDKs;
- remove or pseudonymize private data before storage;
- group repeated errors into issues;
- search Issues, Errors, Logs, Traces, Metrics, Replays, Feedback and Releases
  through one bounded query language;
- download current bounded query results as JSON or CSV;
- receive structured logs, transactions and spans;
- track releases, deploys and release health;
- collect user feedback;
- create dashboards and saved searches;
- send email and Telegram alerts;
- monitor cron jobs and HTTP endpoints;
- collect application counters, gauges and distributions;
- manage users, projects, roles, sessions and API tokens;
- use the Web interface in English or Russian;
- set retention periods and project ingest limits;
- export an Incident Capsule for an issue;
- deliver signed webhooks;
- store files on the local Docker volume;
- symbolicate native and JavaScript stack traces with the Symbolicator container
  included in Medium and High.

Some included features, such as alerts and application metrics, only receive
data after you configure them in Metric or in your SDK.

## Optional or disabled by default

- S3-compatible storage instead of local file storage;
- minidump upload for native crashes;
- debug files and JavaScript source maps (processed by Medium and High);
- cold archive for older data;
- browser Session Replay with `@sentry/browser` 10.66.0.

Minidumps and Session Replay are disabled by default because they need an explicit
privacy decision.

Min disables attachments. Min and Low omit Symbolicator. See
[Capacity and profiles](capacity.md) before selecting a server size.

## Selected next work

- bounded search over cold archive objects without automatic hot restore.

## Outside the current roadmap

- profiling;
- single sign-on, SCIM, MFA and passkeys;
- multiple Metric processing nodes;
- built-in high-availability deployment;
- automatic migration between different database schema generations;
- a Prometheus metrics endpoint.

Metric also provides a machine-readable feature list at
`GET /api/v1/capabilities`.
