# Release readiness fixes

Date: 2026-10-15
Status: implemented and locally verified; release not published

## Scope and publication boundary

Prepare Metric 0.1.8 for review on the current branch. Do not create or push a
release tag, publish images or charts, or create a GitHub Release. Retain the
prerelease wording until publication is separately authorized.

## Accepted work

- [x] Make the Windows installer compatible with PowerShell 5.1 and 7. Treat
  missing volumes as normal, preserve the existing-data guard, and fail on
  Docker daemon errors.
  The full Linux pipeline check additionally exposed Docker consuming the script's
  standard input; service commands now receive `/dev/null`.
- [x] Pass configured monitor-run retention to both maintenance adapters.
  Correct surviving scheduler-created Missed records in bounded, restart-safe
  batches; already deleted records cannot be recovered.
- [x] Reject `retention.replay_archive=true` with an actionable configuration
  error. Document upgrade handling for previously held data. A full replay
  archiver is deferred; do not silently release existing archive holds.
- [x] Report live component health using worker lifetimes and dependency
  readiness. Remove unconditional blob-store availability claims and test
  failure and recovery behavior.
- [x] Correct PHP/Ruby compatibility claims and document installer requirements,
  Replay limitations, and asynchronous expiry behavior.
- [x] Make CI reusable and require successful checks of the release tag's exact
  commit before any release publication job can run.
- [x] Run targeted regression tests and the applicable Rust, Web, SDK,
  documentation, Helm, database and installer checks. Record actual outcomes
  and infrastructure limitations rather than treating skipped tests as passes.

## PostgreSQL retention decision

Keep the current deletion algorithm unless measurement demonstrates unacceptable
behavior. Measure a populated retention cycle, concurrent operations and restart
behavior where infrastructure permits. The approximately 5.6-hour minimum scan
at one million errors on Min is a calculation from polling and batch settings,
not a load-test result. Expiry is asynchronous; database capacity and cleanup
progress matter more than immediate deletion at the deadline.

## Completion evidence

Update this plan with the implemented changes, test results and remaining release
checks. Local build success alone does not establish production readiness.

The monitor repair has its own `monitor_retention` scheduler task and keyset
cursor. It does not delay detection of new missed runs until a history scan ends.
Status uses current task handles and database readiness. Blob storage is explicitly
shown as configured, without claiming that a read/write probe succeeded.

Verified during implementation:

- Windows PowerShell 5.1 and PowerShell 7 volume-guard regression tests passed.
  Full PostgreSQL installation, rerun and secret-preservation checks passed on
  both shells using the previously built local application image.
- Rust workspace run: 370 passed, 0 failed, 273 explicitly ignored infrastructure
  or performance tests. Those ignored tests are tracked separately below.
- The new MongoDB and PostgreSQL monitor-retention regression tests passed on
  real databases, including repeating repair after cursor loss.
- Both real server backends passed account/project/API, event readback, process
  restart, database dump/restore and physical project purge checks, including all
  16 SDK/browser scenarios and populated Replay, attachment, signal and metric data.
- PostgreSQL adapter: 177 infrastructure tests passed. MongoDB adapter: 32 passed
  across the initial run and retries with the required test URI set. Fifteen
  additional real-server integration checks passed. Strict Clippy passed.
- All 16 real SDK compatibility tests passed. Web: 117 unit tests and 40 browser
  tests passed; two reference-render tests were explicitly skipped. Lint and build
  passed. Documentation build and validation passed.
- Helm validation passed 11 configurations and rejected 22 invalid cases.
  Release-workflow dependency checks passed without publishing anything.
- PostgreSQL retention diagnostic passed with 1,000,000 expired error records,
  batch size 100, a deliberate cursor reset and concurrent writes to another
  project. The cycle used 20,026 calls and took 343,596 ms without scheduler
  sleeps; the maximum observed concurrent-write time was 248 ms. All synthetic
  errors were deleted. This Windows/Docker diagnostic ran alongside other checks;
  it is not a production throughput or latency guarantee. Keep the deletion
  algorithm unchanged. Polling delays apply separately to scan and deletion
  batches, so a fully expired table takes longer than the initial traversal.

Final container and installer evidence:

- The final-source local image built successfully and passed 12 container lifecycle
  checks, including database restart, graceful shutdown and event readback.
- Offline TCP and verified-TLS runs passed 24 checks. All eight supplied backend
  profiles passed actual configuration validation.
- Eight unattended installation scenarios passed across Linux shell (Ubuntu/WSL)
  and PowerShell: legacy pipeline, bundled PostgreSQL, external PostgreSQL and
  external MongoDB. Both shells passed the two external TLS scenarios, including
  required client certificates and rejected invalid credentials/certificates.
- Interactive PowerShell bundled-PostgreSQL and Linux external-PostgreSQL wizard
  checks passed. PowerShell 5.1 also passed its separate full install/rerun check.
- The additional pipeline-input regression passed with a synthetic Docker CLI
  that deliberately drains stdin. The real `curl | sh` scenario passed after the
  script was enclosed in a parsed function and child stdin was isolated.

Full logs are in `target/release-fixes-*.log`. Earlier attempts include a Docker
Hub rate limit, a PostgreSQL test-runner timeout including compilation, a large
diagnostic seed insert exceeding statement timeout, and missing Mongo test URI
variables. Successful reruns are recorded above; none is counted as a silent skip.

## Final review before the local commit

Reviewed every changed production file, regression test, installer, workflow and
documentation change again. No additional blocking logic defect was found.
Corrected two remaining README descriptions that mentioned only MongoDB despite
the PostgreSQL backend being supported.

Repeated checks passed for documentation consistency and build, compatibility,
deployment profiles, installer asset synchronization, actual-shell volume guards,
same-commit release workflow dependencies, Rust formatting and patch whitespace.
The Web unit suite passed again: 117 tests across 33 files.
The full Rust workspace/all-targets/all-features run passed again: 370 passed,
0 failed, 273 explicitly ignored infrastructure/performance tests across 48
test binaries. The real infrastructure results above remain separately recorded.
Logs for the repeat runs are in `target/release-review-*.log`.

Release assessment: the agreed fixes are implemented and locally validated.
Successful hosted CI on the final release commit is still required before
publication; this local review does not claim that hosted run has occurred.

## Authorized publication attempt

The user subsequently authorized merging into main and publishing 0.1.8, with a
stop on substantive CI failures. PR #12 ran all seven CI jobs. Six succeeded;
the remaining job passed container lifecycle, offline TCP/TLS and eight profile
checks, then exposed an overly specific negative-test exit-code assertion.
Ubuntu dash exits 2 when its special `exec` builtin cannot open `/dev/tty`;
Bash exits 1. Both refuse installation before creating secrets. The test now
accepts 1 or 2 only for that no-terminal shell case. Other negative cases retain
their exact exit-code assertions. Reproduced on Ubuntu dash, and repeated negative
checks passed on Git Bash and PowerShell. Production code was unchanged.

The next CI run passed that assertion and all eight unattended installation
scenarios, then exposed the POSIX terminal driver's missing console emulation in
the PowerShell wizard case. A standalone three-`Read-Host` script reproduced the
hang without any installer or Docker operations. Giving the PTY a 40x180 size and
answering console cursor-position requests fixed the reproduction on actual Linux
PowerShell 7.6.7. A fast CI regression exercises both file and pipeline invocation
before the expensive builds. This correction also changes only test code.

## Publication state at the original readiness review

No Git tag, push, remote image/chart publication or GitHub Release was performed.
The new release workflow will require the exact release commit to pass reusable
CI before publication. Native macOS/ARM64 validation and production load/soak
claims are not established by these local checks. Replay archiving remains
deferred, and the existing PostgreSQL expiry algorithm is retained.
