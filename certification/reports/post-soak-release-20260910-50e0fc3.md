# TorrentNG Post-Soak Release Gate

- Date UTC: 2026-09-10T19:38:01Z
- Commit: 50e0fc3
- Report directory: /home/keith/Documents/code/TorrentNG/certification/reports

## Checks

| Gate | Status | Evidence |
|---|---|---|
| soak finalization | PASS | soak-final-public-debian-20260910.md |
| local release gate | WARN | local-release-20260910-50e0fc3.md; PASS_WITH_WARNINGS |
| storage release certification | PASS | storage-release-certification-kspls0-lvm-20260910-b393eb0.md |
| memory roadmap certification | PASS | memory-roadmap-certification-20260910-b393eb0.md |
| storage certification index | PASS | storage-certification-index.md; hardware, io_uring, and move/import evidence present |
| certification status rollup | WARN | Universal compatibility=PASS_WITH_SKIPS;Universal live compatibility=PASS_WITH_SKIPS;Local release gate=PASS_WITH_WARNINGS |

## Boundaries

- This gate rolls up the latest generated evidence. It does not replace a fresh real-device run on new release hardware.
- Memory roadmap WARN rows are allowed only when the warning is an explicit non-claim, such as physical PV affinity or host-scale evidence outside the current release target.

Overall status: PASS_WITH_WARNINGS
Warnings: 2
