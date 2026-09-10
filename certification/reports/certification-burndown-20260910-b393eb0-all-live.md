# TorrentNG Certification Burndown

- Date UTC: 2026-09-10T19:27:00Z
- Commit: b393eb0
- Report directory: /home/keith/Documents/code/TorrentNG/certification/reports

## Non-Clean Rows

| Gate | Status | Latest report | Action |
|---|---|---|---|
| Universal compatibility | PASS_WITH_SKIPS | universal-compat-b393eb0-all-live.md | Rerun `scripts/universal_compatibility_certification.sh` with the skipped optional legs enabled as needed: `UNIVERSAL_COMPAT_LIVE=1` for Docker client interop, `UNIVERSAL_COMPAT_PUBLIC=1` for approved public torrent downloads, `UNIVERSAL_COMPAT_REAL_DEVICE=1 TNG_STORAGE_BENCH_DIR=/mnt/target` for target storage hardware, and `UNIVERSAL_COMPAT_MOBILE=1` for the mobile read-flow leg. |
| Universal live compatibility | PASS_WITH_SKIPS | universal-live-local-public-debian-20260910-b393eb0.md | Latest universal-live report `universal-live-local-public-debian-20260910-b393eb0.md` may already include a passing local Docker interop leg; skipped rows are external only unless the report says otherwise. Rerun with `UNIVERSAL_LIVE_PUBLIC=1` for approved public torrents and/or `UNIVERSAL_LIVE_REAL_DEVICE=1 TNG_STORAGE_BENCH_DIR=/mnt/target` for target storage hardware. |
| Local release gate | PASS_WITH_WARNINGS | local-release-backend-burndown-final-20260904.md | Rerun `scripts/local_release_gate.sh` after warning rows are resolved. Set `TNG_STORAGE_MATRIX_TARGETS` for real-device storage release probes. |
| Post-soak release gate | PASS_WITH_WARNINGS | post-soak-release-20260910-b393eb0.md | Rerun `scripts/post_soak_release_gate.sh` after all upstream warning rows are resolved. |

Non-clean rows: 4

Overall status: PASS_WITH_ACTIONS
