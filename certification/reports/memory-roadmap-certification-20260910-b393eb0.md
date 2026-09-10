# TorrentNG Memory Roadmap Certification

- Generated: 2026-09-10T18:19:23Z
- Host: kspld0
- Commit: b393eb0

| Roadmap item | Status | Evidence |
| --- | --- | --- |
| 10k/100k idle RSS/task/fd proxy | PASS | certification/reports/local-release-backend-burndown-final-20260904.md |
| 1k hot seeding memory-cap proxy | PASS | certification/reports/local-release-backend-burndown-final-20260904.md |
| slow-disk/fast-peer backpressure proxy | PASS | certification/reports/local-release-backend-burndown-final-20260904.md |
| hard queued-disk memory leases | PASS | certification/reports/local-release-backend-burndown-final-20260904.md |
| process-level per-device queue registry | PASS | certification/reports/local-release-backend-burndown-final-20260904.md |
| move/import/delete executor safety | PASS | certification/reports/storage-move-import-kspls0-lvm-20260910-b393eb0.md |
| real-root move/import fixture evidence | PASS | certification/reports/storage-move-import-kspls0-lvm-20260910-b393eb0.md |
| HDD 5x elevator release evidence | PASS | certification/reports/storage-hardware-kspls0-lvm-20260910-b393eb0.md |
| sampled LVM physical-PV placement evidence | PASS | certification/reports/storage-hardware-kspls0-lvm-20260910-b393eb0.md |
| io_uring real-device capability probe | PASS | certification/reports/storage-uring-graduation-kspls0-lvm-20260910-b393eb0.md |
| io_uring frame-pool slot graduation | PASS | certification/reports/storage-uring-graduation-kspls0-lvm-20260910-b393eb0.md |
| storage release certification wrapper | PASS | certification/reports/storage-release-certification-kspls0-lvm-20260910-b393eb0.md |

## Boundaries

- Deterministic LVM physical-drive placement remains a non-claim unless a lower-level PV-targeted path is added.
- io_uring remains explicit opt-in until graduation reports meet selected-backend, registered-file, frame-pool-slot strategy, and throughput thresholds on target hardware.
- Multi-TB move/import certification remains host/run evidence; use the real-root fixture knobs to scale the report on the target storage root.
