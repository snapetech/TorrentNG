# TorrentNG Release Evidence Runbook

This is the operator sequence for turning the current certification state into a
strict release-ready state.

The repository CI gate is currently green on `main`: GitHub Actions run
`33915548520` passed all ten jobs on `f1c39fd`, and the dynamic CodeQL
orchestration run `33915547352` also passed all four analyses. This closes hosted repository
execution; it does not by itself configure branch protection or certify public
network, target-device, or 24-hour-soak behavior.

The concrete failure history and fixes for recent hosted regressions are
tracked in [`CI_FAILURE_BURN_DOWN.md`](CI_FAILURE_BURN_DOWN.md).

## Local Refresh

Run the deterministic local gates first:

```sh
scripts/external_evidence_preflight.sh
scripts/certification_status_json.sh
scripts/universal_compatibility_certification.sh
scripts/migration_corpus_certification.sh
scripts/local_release_gate.sh
scripts/post_soak_release_gate.sh
scripts/certification_burndown.sh
```

The local release gate may report `PASS_WITH_WARNINGS` while external evidence
is missing. That is expected before the next sections are complete.
Use the external preflight report to check whether the current host has Docker,
public-transfer opt-in, corpus files, a writable storage target, and an active
24h soak before launching long gates.
For CI or release-blocking host validation, promote preflight warnings to
failures:

```sh
TNG_EXTERNAL_PREFLIGHT_STRICT=1 scripts/external_evidence_preflight.sh
```

## Migration Corpus

The repository includes generated corpus fixtures for every supported source
family:

```text
testdata/migration-corpus/qbittorrent/
testdata/migration-corpus/transmission/
testdata/migration-corpus/deluge/
testdata/migration-corpus/utorrent/
testdata/migration-corpus/biglybt/
testdata/migration-corpus/tixati/
testdata/migration-corpus/rtorrent/
testdata/migration-corpus/generic/
```

Enforce the generated corpus and manifest:

```sh
TNG_REQUIRE_MIGRATION_CORPUS=1 scripts/migration_corpus_certification.sh
```

The report includes SHA-256 hashes for every discovered artifact. In strict
mode, `manifest.toml` is mandatory, each source family must declare at least
one artifact, every declared artifact must stay under its matching family
directory, and every discovered evidence file must be declared with source and
permission metadata. Declared `sha256` values are verified when present. Add
real exported client artifacts beside the generated fixtures when a release
needs extra version-specific evidence.

## Live Compatibility

Run Docker client interop:

```sh
scripts/universal_live_certification.sh
```

Enable public legal torrent downloads only when that is allowed for the release
environment:

```sh
UNIVERSAL_LIVE_PUBLIC=1 scripts/universal_live_certification.sh
```

Run real-device storage evidence on target hardware:

```sh
UNIVERSAL_LIVE_REAL_DEVICE=1 \
TNG_STORAGE_BENCH_DIR=/mnt/target \
scripts/universal_live_certification.sh
```

## 24h Soak

Start the long soak:

```sh
scripts/start_24h_soak.sh
```

Check launcher preconditions without starting the background job:

```sh
TNG_24H_SOAK_DRY_RUN=1 scripts/start_24h_soak.sh
```

Monitor it:

```sh
scripts/soak_status.sh
```

The soak runner records health and qBit sync HTTP status plus process RSS. A
passing report is required for a strict production-readiness claim; a short
local run is useful for smoke coverage but cannot substitute for the configured
24-hour sample window or target-device evidence.

Finalize it after completion:

```sh
SOAK_MIN_SAMPLES=1200 RESTORE_NORMAL=1 scripts/finalize_soak.sh <soak-24h-report>
```

## Strict Readiness

When the warning rows are resolved, run:

```sh
scripts/release_readiness_gate.sh
```

This gate fails on any `FAIL`, `MISSING`, `PASS_WITH_*`, `SKIP`,
`STALE/INCOMPLETE`, or running/unknown certification row. Use the paired
burndown report for exact remediation.

For a local-only release scope where public-swarm, real-device, and 24h soak
evidence are intentionally out of scope, run:

```sh
TNG_RELEASE_SCOPE=local scripts/release_readiness_gate.sh
```

This does not mark external evidence as passed. It removes the documented
opt-in rows from the blocking set for that readiness report while leaving them
visible in `scripts/certification_status.sh` and the burndown report. Use the
default strict scope for public releases.

To refresh status, burndown, strict readiness, and the evidence bundle in one
release-blocking command, run:

```sh
scripts/release_evidence_suite.sh
```

The burndown/readiness/post-soak policy intentionally ignores meta-report rows
such as certification bundle, burndown, readiness, JSON status, and the evidence
suite itself when deciding whether product evidence is clean.

## Evidence Bundle

Package the latest status and referenced reports:

```sh
scripts/certification_bundle.sh
```

The output tarball is written under `certification/bundles/`, and its generated
report includes the bundle, manifest, and status SHA-256 values. If any report
referenced by certification status is missing at packaging time, the bundle
report is downgraded to `PASS_WITH_WARNINGS`.

`scripts/certification_status_json.sh` writes a machine-readable
`certification-status-*.json` file plus a companion Markdown report for the
status table. The companion report is `PASS` when every row is clean,
`PASS_WITH_WARNINGS` when rows are warning-only, and `FAIL` when any row is
failed, missing, or otherwise invalid; the command exits non-zero for the last
case so CI and release automation cannot mistake a failed status table for a
passing export.
