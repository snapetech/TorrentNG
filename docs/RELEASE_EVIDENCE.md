# TorrentNG Release Evidence Runbook

This is the operator sequence for turning the current certification state into a
strict release-ready state.

The current worktree qualification is at `1dea3ea`; older runtime product and
LVM evidence remains tied to the commit recorded by each report. GitHub
Actions evidence cited below is historical hosted-run evidence; CI success does
not configure branch protection or certify broader public-network, target-device,
or scale behavior.

## Current qualification scope — 2026-09-16

Numeric torrent-count capacity proofs, including 10k/100k synthetic or
hot-set runs, are removed from the required release scope. Existing reports and
tests remain historical or diagnostic evidence and must not be summarized as a
capacity certificate. The current qualification also defers the 24-hour soak;
run the non-soak gates with `TNG_DEFER_24H_SOAK=1` and record the deferral
explicitly. A later soak must target the then-current artifact before making a
stability claim.

The concrete failure history and fixes for recent hosted regressions are
tracked in [`CI_FAILURE_BURN_DOWN.md`](CI_FAILURE_BURN_DOWN.md).

## Current qualification update — 2026-09-16

The current qualification is intentionally narrower than a production-scale
certificate. Numeric torrent-count proofs are out of scope, and the 24-hour
soak is explicitly deferred with `TNG_DEFER_24H_SOAK=1`.

| Gate | Result | Evidence |
| --- | --- | --- |
| Local Docker interoperability | PASS | [`interop-matrix-current-local-20260916.md`](../certification/reports/interop-matrix-current-local-20260916.md); 28/28 rows pass across qBittorrent, Transmission, Deluge, and rTorrent, including protocol and recovery rows |
| Public Debian transfer | PASS | [`interop-matrix-current-public-debian-20260916.md`](../certification/reports/interop-matrix-current-public-debian-20260916.md); official Debian 13.7 netinst, 792,723,456 bytes, 143 Rust peers observed |
| Public Ubuntu transfer | FAIL/UNQUALIFIED | [`interop-matrix-current-public-ubuntu-bounded-20260916.md`](../certification/reports/interop-matrix-current-public-ubuntu-bounded-20260916.md); official Ubuntu 26.04.1, bounded at 120 seconds, no completed payload |
| Public Fedora transfer | FAIL/UNQUALIFIED | [`interop-matrix-current-public-fedora-bounded-20260916.md`](../certification/reports/interop-matrix-current-public-fedora-bounded-20260916.md); official Fedora Server 45 Beta, bounded at 120 seconds, no completed payload |
| Live fault-containment matrix | PASS | [`backend-burndown-fault-matrix-current-20260916.md`](../certification/reports/backend-burndown-fault-matrix-current-20260916.md); live crash/restart, SQLite failure/recovery, cancellation, and filesystem isolation pass |
| Storage lab certification | PASS | [`storage-release-certification-lab-current-20260916.md`](../certification/reports/storage-release-certification-lab-current-20260916.md); temporary tmpfs root only, not physical-device evidence |
| External evidence preflight | PASS_WITH_WARNINGS | [`external-evidence-preflight-current-20260916.md`](../certification/reports/external-evidence-preflight-current-20260916.md); real-device target unavailable, soak informationally deferred |
| Local release readiness | PASS | [`release-readiness-local-current-20260916.md`](../certification/reports/release-readiness-local-current-20260916.md) |
| Strict release readiness | FAIL | [`release-readiness-current-20260916.md`](../certification/reports/release-readiness-current-20260916.md); external policy rows remain non-clean |

The 2026-09-16 public failures are recorded as qualification results, not
capacity proofs. They do not show a TorrentNG protocol failure: neither source
reached the harness completion/hash gate within the explicit bounded window.

## Historical qualification update — 2026-09-10

The following external gates are now recorded in the repository. The storage
and current local/public live reports are tied to pushed commit `b393eb0`; the
counted named public soak source predates the b393 source refresh and its raw
report does not record a source commit.

| Gate | Result | Evidence |
| --- | --- | --- |
| Public Debian transfer | PASS | [`interop-matrix-20260910T192200Z.md`](../certification/reports/interop-matrix-20260910T192200Z.md); canonical parent [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md), 142 peers observed |
| Canonical all-live compatibility | PASS_WITH_SKIPS | [`universal-compat-b393eb0-all-live.md`](../certification/reports/universal-compat-b393eb0-all-live.md); local Docker, mobile, and public Debian pass; the separately certified real-device storage wrapper is skipped |
| Public Debian 24-hour soak | DEFERRED | The prior named run remains archival evidence; the current qualification makes no 24-hour stability claim |
| kspls0 LVM storage release certification | PASS | [`storage-release-certification-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/storage-release-certification-kspls0-lvm-20260910-b393eb0.md); HDD median 5.11x, LVM extent, io_uring, and real-root move/import checks |
| kspls0 real-device storage matrix | PASS_WITH_SKIPS | [`universal-live-kspls0-lvm-20260910-b393eb0.md`](../certification/reports/universal-live-kspls0-lvm-20260910-b393eb0.md); targeted storage leg passes, unrelated live legs are explicit skips |
| Strict external preflight | PASS | [`external-evidence-preflight-release-strict-20260910-b393eb0.md`](../certification/reports/external-evidence-preflight-release-strict-20260910-b393eb0.md) |
| Clean release-binary smoke | PASS | [`backend-burndown-native-release-smoke-20260910-final.md`](../certification/reports/backend-burndown-native-release-smoke-20260910-final.md); build commit `3cb0ba4`, 22,449,216 bytes, SHA-256 `7fbac478b696316d989028c47573e4cd248f04a98a479f218017c0ec5a812b5e`, 457 ms |
| Full local release gate | PASS_WITH_WARNINGS | [`local-release-20260910-50e0fc3.md`](../certification/reports/local-release-20260910-50e0fc3.md); all configured gates pass, with one explicit local block-device skip |
| WebUI certification | PASS | [`webui-certification-20260910-50e0fc3.md`](../certification/reports/webui-certification-20260910-50e0fc3.md); production build, lint, and browser matrix pass |

The storage release report and all current child reports record commit `b393eb0`.
These results qualify the exercised Debian torrent and kspls0 LVM target; they
do not turn one public source into universal compatibility or one storage host
into a fleet-wide capacity claim. The prior counted soak remains archival
evidence for its named run, but its raw source predates b393 and is not a
current release soak.

The machine-readable final rollup is
[`certification-status-20260910-50e0fc3.json`](../certification/reports/certification-status-20260910-50e0fc3.json),
with the action ledger in
[`certification-burndown-20260910-50e0fc3-final.md`](../certification/reports/certification-burndown-20260910-50e0fc3-final.md)
and strict readiness result in
[`release-readiness-20260910-50e0fc3-final.md`](../certification/reports/release-readiness-20260910-50e0fc3-final.md).
The strict readiness result is intentionally FAIL while the four explicit
non-clean policy rows remain.

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
public-transfer opt-in, corpus files, and a writable storage target before
launching live gates. The current qualification deliberately omits the long
soak; set `TNG_DEFER_24H_SOAK=1` so the report records that policy.
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

The named Debian transfer is complete. Use the transfer report in the current
qualification update above as evidence; the prior soak is archival. Rerun the
commands when a new artifact or materially different configuration needs fresh
qualification.

Run real-device storage evidence on target hardware:

```sh
UNIVERSAL_LIVE_REAL_DEVICE=1 \
TNG_STORAGE_BENCH_DIR=/mnt/target \
scripts/universal_live_certification.sh
```

## 24h Soak

The named Debian run completed successfully in an earlier qualification. Its live source report is
[`soak-24h-public-debian-20260905-v3.md`](../.run/soak-24h-public-debian-20260905-v3.md),
and its finalization is
[`soak-final-public-debian-20260910.md`](../certification/reports/soak-final-public-debian-20260910.md).
The finalizer accepted 1,437 samples, one matching completed torrent, healthy
HTTP/dependency responses, and the configured resource ceilings. That result is
retained as archival evidence; it is not a current-artifact release soak.

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
passing report is required before making a strict production-readiness or
stability claim. A short local run is useful for smoke coverage but cannot
substitute for the configured 24-hour sample window or target-device evidence.

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
