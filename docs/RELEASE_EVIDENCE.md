# TorrentNG Release Evidence Runbook

This is the operator sequence for turning the current certification state into a
strict release-ready state.

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

## Exported Migration Corpus

Populate the real exported client corpus:

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

Then enforce it:

```sh
TNG_REQUIRE_MIGRATION_CORPUS=1 scripts/migration_corpus_certification.sh
```

The report includes SHA-256 hashes for every discovered artifact.

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
status table.
