# TorrentNG Release Evidence Runbook

This is the operator sequence for turning the current certification state into a
strict release-ready state.

The current pushed product revision has green hosted gates: GitHub Actions run
`34510889406` passed all ten jobs and dynamic CodeQL run `34510889093` passed
all four analyses on `b393eb0`. CI success does not configure branch protection
or certify broader public-network, target-device, or scale behavior.

The concrete failure history and fixes for recent hosted regressions are
tracked in [`CI_FAILURE_BURN_DOWN.md`](CI_FAILURE_BURN_DOWN.md).

## Current qualification update — 2026-09-10

The following external gates are now recorded in the repository. The storage
and current local/public live reports are tied to pushed commit `b393eb0`; the
counted named public soak source predates the b393 source refresh and its raw
report does not record a source commit.

| Gate | Result | Evidence |
| --- | --- | --- |
| Public Debian transfer | PASS | [`interop-matrix-20260910T192200Z.md`](../certification/reports/interop-matrix-20260910T192200Z.md); canonical parent [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md), 142 peers observed |
| Canonical all-live compatibility | PASS_WITH_SKIPS | [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md); local Docker, mobile, and public Debian pass; the separately certified real-device storage wrapper is skipped |
| Public Debian 24-hour soak | PASS | [`soak-final-public-debian-20260910.md`](../certification/reports/soak-final-public-debian-20260910.md); 1,437 samples and one exact completed torrent |
| kspls0 LVM storage release certification | PASS | [`storage-release-certification-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/storage-release-certification-kspls0-lvm-20260910-b393eb0.md); HDD median 5.11x, LVM extent, io_uring, and real-root move/import checks |
| kspls0 real-device storage matrix | PASS_WITH_SKIPS | [`universal-live-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/universal-live-kspls0-lvm-20260910-b393eb0.md); targeted storage leg passes, unrelated live legs are explicit skips |
| Strict external preflight | PASS | [`external-evidence-preflight-release-strict-20260910-b393eb0.md`](../certification/reports/external-evidence-preflight-release-strict-20260910-b393eb0.md) |

The storage release report and all current child reports record commit `b393eb0`.
These results qualify the exercised Debian torrent and kspls0 LVM target; they
do not turn one public source into universal compatibility or one storage host
into a fleet-wide capacity claim. The counted soak remains valid evidence for
the named run, but its raw source predates b393 and is not a b393 release soak.

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

For the direct Debian public matrix, preserve the completed payload for a
named soak:

```sh
INTEROP_PUBLIC_ONLY=debian \
INTEROP_KEEP_STACK=1 \
INTEROP_KEEP_PUBLIC_DATA=1 \
scripts/interop_matrix.sh --public
```

The named Debian transfer and soak are already complete. Use the reports in the
current qualification update above as the release evidence; rerun the commands
only when a new artifact or materially different configuration needs fresh
qualification.

Run real-device storage evidence on target hardware:

```sh
UNIVERSAL_LIVE_REAL_DEVICE=1 \
TNG_STORAGE_BENCH_DIR=/mnt/target \
scripts/universal_live_certification.sh
```

## 24h Soak

The named Debian run completed successfully. Its live source report is
[`soak-24h-public-debian-20260905-v3.md`](../.run/soak-24h-public-debian-20260905-v3.md),
and its finalization is
[`soak-final-public-debian-20260910.md`](../certification/reports/soak-final-public-debian-20260910.md).
The finalizer accepted 1,437 samples, one matching completed torrent, healthy
HTTP/dependency responses, and the configured resource ceilings. The collected
systemd unit is absent by design after completion.

Start the long soak:

```sh
scripts/start_24h_soak.sh
```

For a public-torrent soak, set `SOAK_EXPECTED_TORRENT_NAME` and
`SOAK_EXPECTED_TORRENT_HASH`. The runner then requires that exact completed
torrent in every sample. When a user systemd manager is available, the launcher
uses it as a supervised process with no automatic restart, so a failed soak
cannot be silently replaced by a fresh report; otherwise it uses a detached
session.

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
