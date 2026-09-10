# TorrentNG Native Release-Binary Smoke

- Date UTC: 2026-09-10T19:30:13Z
- Host: kspld0
- Commit: 3cb0ba4
- Binary: target/release/torrentngd
- Binary bytes: 22449216
- Binary SHA-256: 7fbac478b696316d989028c47573e4cd248f04a98a479f218017c0ec5a812b5e
- Config: /home/keith/Documents/code/TorrentNG/certification/reports/backend-burndown-native-config-20260902.toml
- Worktree: clean

This run exercised the optimized production daemon binary with the
authenticated native and qBittorrent facades, then sent SIGTERM and
waited for process exit. It is a deployment smoke test, not 100k-scale
capacity evidence.

## Checks

| Check | Result | Evidence |
|---|---|---|
| Startup and health | PASS | backend-burndown-native-release-smoke-20260910-final.health.json |
| Native list envelope | PASS | backend-burndown-native-release-smoke-20260910-final.torrents.json |
| Native transfer info | PASS | backend-burndown-native-release-smoke-20260910-final.transfer.json |
| qBittorrent list | PASS | backend-burndown-native-release-smoke-20260910-final.qbit.json |
| qBittorrent transfer info | PASS | backend-burndown-native-release-smoke-20260910-final.qbit-transfer.json |
| Prometheus metrics | PASS | backend-burndown-native-release-smoke-20260910-final.metrics.txt; 647 lines / 43459 bytes |
| SIGTERM and clean exit | PASS | backend-burndown-native-release-smoke-20260910-final.log; 2 polls |

- Total smoke duration milliseconds: 457

Overall status: PASS
