# TorrentNG Release Evidence Runbook

This runbook records older qualification results and the operator sequence for
establishing release readiness on an exact source revision.

The previously published worktree qualification snapshot is at `1dea3ea`; older runtime product and
LVM evidence remains tied to the commit recorded by each report. GitHub Actions
evidence cited below is historical hosted-run evidence. A `main` ruleset was
added on 2026-10-06 to require pull requests and named CI checks; it has not yet
been exercised by a PR from this checkout. Neither CI nor branch protection
certifies broader public-network, target-device, or scale behavior.

## Current checkout status — 2026-10-06

The changes on `codex/credibility-and-release-prep` have not been tested or
certified. The reports below are historical records and do not qualify this
checkout. The 2026-10-02 physical-storage report records source commit
`db4e333c` and a dirty worktree. The 24-hour soak report records its container
and image IDs but no source commit; its final report also contains a machine-
specific absolute path. Treat that soak as a historical idle-service
observation, not source-bound release evidence. Run the exact-SHA checks in the
[at-home validation plan](AT_HOME_VALIDATION.md) before considering a release.

## Source safeguards recorded — 2026-10-02

The soak sampler now validates the `torrentngd` process and records its
container/image identity. Storage hardware rollups now require a block-backed,
non-pseudo, non-network target; a tmpfs-only run is smoke evidence and yields a
hardware-gate SKIP. These source changes did not create soak or physical-device
evidence by themselves. The historical local reports below remain tied to their
recorded environments; exact-source follow-up is pending.

## Historical local qualification — 2026-10-02

These results belong to the named reports and their recorded environments. They
are not evidence for the current checkout.

| Gate | Result | Evidence and scope |
| --- | --- | --- |
| Physical storage release certification | PASS | [`storage-release-certification-local-ssd-20261002.md`](../certification/reports/storage-release-certification-local-ssd-20261002.md); `/dev/sdb`, Btrfs, physical SSD. Hardware qualification, `io_uring` graduation, real-root move/import, and storage index passed. The matrix exercised 256 MiB backend streams and a 16 MiB recheck; HDD-only elevator evidence is not applicable to this target. |
| Named WebUI certification | PASS | [`webui-certification-tng147-local-20261002.md`](../certification/reports/webui-certification-tng147-local-20261002.md); worktree state is recorded as dirty; production build, lint, and browser matrix pass (28 passed, 10 skipped). |
| One-sample daemon sampler smoke | PASS | Authenticated smoke against the daemon recorded `torrentngd` RSS/FD/thread data and healthy API/dependency checks. It is not 24-hour evidence. |
| Named 24-hour idle-daemon run | PASS | [soak-24h-tng145-current-20261002.md](../certification/reports/soak-24h-tng145-current-20261002.md) and [soak-final-tng145-current-20261003.md](../certification/reports/soak-final-tng145-current-20261003.md); PASS (1439 samples; 2026-10-02T21:11:04Z through 2026-10-03T21:17:37Z). The raw report has no source commit. Empty state, DHT disabled, no torrent workload; idle-daemon observation only, not evidence for this checkout, a loaded swarm, or public-network behavior. |

The daemon image is identified by its recorded Docker image ID, but the raw
soak report does not record a source revision. The idle soak can establish idle
process ceilings and continuity only; it does not replace a loaded
public-torrent soak.

## Qualification scope decision — 2026-09-16

Numeric torrent-count capacity proofs, including 10k/100k synthetic or
hot-set runs, are removed from the required release scope. Existing reports and
tests remain historical or diagnostic evidence and must not be summarized as a
capacity certificate. A later named idle-daemon run is recorded above and in
[soak-final-tng145-current-20261003.md](../certification/reports/soak-final-tng145-current-20261003.md).
Its source revision is not recorded, and its scope is limited to an idle daemon;
it does not establish loaded-swarm stability or qualify this checkout.

The concrete failure history and fixes for recent hosted regressions are
tracked in [`CI_FAILURE_BURN_DOWN.md`](CI_FAILURE_BURN_DOWN.md).

## Qualification snapshot — 2026-09-16 (historical)

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
| Storage lab certification | SMOKE_ONLY | [`storage-release-certification-lab-current-20260916.md`](../certification/reports/storage-release-certification-lab-current-20260916.md); raw child PASS was on tmpfs and does not qualify physical storage |
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
