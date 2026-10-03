# TorrentNG Soak Finalization

- Date UTC: 2026-10-03T21:18:00Z
- Source report: /home/keith/Documents/code/TorrentNG/certification/reports/soak-24h-tng145-current-20261002.md
- Minimum samples: 1200
- Max RSS MB: 500
- Max file descriptors: 4096
- Max threads: 512
- Minimum disk free MB: 100

## Checks

| Check | Result | Detail |
|---|---|---|
| source report | PASS | /home/keith/Documents/code/TorrentNG/certification/reports/soak-24h-tng145-current-20261002.md |
| sample count | PASS | 1439 >= 1200 |
| memory ceiling | PASS | 74.6MB <= 500MB |
| health samples | PASS | all HTTP 200 |
| sync samples | PASS | all HTTP 200 |
| file-descriptor ceiling | PASS | 18 <= 4096 |
| thread ceiling | PASS | 48 <= 512 |
| disk-free floor | PASS | 1024MB >= 100MB |
| metrics samples | PASS | all HTTP 200 |
| dependency health fields | PASS | no unhealthy or unknown fields |
| source completion | PASS | source report completed PASS |
| restore normal sync | INFO | set RESTORE_NORMAL=1 to restore certification service |

Overall status: PASS
