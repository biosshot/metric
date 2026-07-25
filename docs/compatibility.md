# Compatibility

Metric version one claims Sentry compatibility only for the Error Event path and
only for exact rows marked `pass` in
`compatibility/sentry-sdk-matrix.toml`.

Currently verified:

- official `@sentry/browser` 10.66.0 through Chromium 149.0.7827.55;
- official `@sentry/node` 10.66.0 through a real Node process, with separate base
  Error and safe JSON attachment gates;
- official Python `sentry-sdk` 2.32.0 through a real CPython 3.11 process;
- official Java `sentry-java` 8.50.1 through a real Java 25 process;
- official .NET `Sentry` 6.7.0 through a real .NET 9 process;
- official Go `sentry-go` 0.48.0 through a real Go 1.25.1 process;
- official Rust `sentry` 0.48.5 through a real Rust 1.88.0 process;
- `sentry-cli` 3.6.2 and 2.58.6 debug-file and Artifact Bundle contracts.

Python, Java and .NET are the Phase 22 release-required SDK families. The remaining
seven untested inventory families are not implicitly supported and do not block this
selected version-one scope.
The fail-closed validator is:

```text
python scripts/validate-compatibility.py
python scripts/validate-compatibility.py --require-all
```

The second command is the final release gate and intentionally fails while any
required family remains untested.

Transactions, spans, sessions, profiles, replays, check-ins, metrics, logs and
feedback are disabled. Native minidump, debug-file, Artifact Bundle, attachment,
Incident Capsule, webhook and cold-archive capabilities are separate contracts and
are advertised by `/api/v1/capabilities`.
