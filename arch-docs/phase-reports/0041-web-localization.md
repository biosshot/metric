# Phase 41 report: English/Russian Web localization

- Status: Complete
- Date: 2026-08-01
- Owner: Web presentation layer
- Decision: ADR-0047

## Delivered

- `vue-i18n` owns user-visible Web strings.
- English is the built-in fallback and Russian is loaded lazily.
- The selected locale is persisted locally and updates the document language.
- Date, time and numeric rendering uses the active locale.
- The language selector is available from settings.
- Shell, authentication, observability, investigation, dashboard, alert, monitor,
  project and administration surfaces use localization keys.
- API codes, SDK field names, stack traces, user values and operational logs remain
  stable untranslated technical data.

## Verification evidence

- locale selection/persistence test;
- every English and Russian message is compiled in the localization test;
- lazy locale catalogs are production-build boundaries;
- Web unit, format, lint and production-build gates remain the owning regression
  suite.

Phase 41 adds no MongoDB collection, schema generation, worker or backend contract.
