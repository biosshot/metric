# Compatibility

Faultkeep version one claims Sentry compatibility only for the Error Event path and
only for exact rows marked `pass` in
`compatibility/sentry-sdk-matrix.toml`.

Currently verified:

- official `@sentry/browser` 10.66.0 through Chromium 149.0.7827.55;
- official `@sentry/node` 10.66.0 through a real Node process, with separate base
  Error and safe JSON attachment gates;
- captured official Python `sentry-sdk` 2.32.0 Error Event fixture;
- `sentry-cli` 3.6.2 and 2.58.6 debug-file and Artifact Bundle contracts.

All other ADR-0036 SDK families are explicitly `untested`, not implicitly supported.
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
