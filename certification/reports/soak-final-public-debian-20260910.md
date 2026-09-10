# TorrentNG Soak Finalization

- Date UTC: 2026-09-10T16:43:34Z
- Source report: .run/soak-24h-public-debian-20260905-v3.md
- Minimum samples: 1200
- Minimum torrents: 1
- Max RSS MB: 500
- Max file descriptors: 4096
- Max threads: 512
- Minimum disk free MB: 100

## Checks

| Check | Result | Detail |
|---|---|---|
| source report | PASS | .run/soak-24h-public-debian-20260905-v3.md |
| sample count | PASS | 1437 >= 1200 |
| torrent floor | PASS | 1 >= 1 |
| memory ceiling | PASS | 1.3MB <= 500MB |
| health samples | PASS | all HTTP 200 |
| sync samples | PASS | all HTTP 200 |
| file-descriptor ceiling | PASS | 3 <= 4096 |
| thread ceiling | PASS | 1 <= 512 |
| disk-free floor | PASS | 14382MB >= 100MB |
| metrics samples | PASS | all HTTP 200 |
| dependency health fields | PASS | no unhealthy or unknown fields |
| source completion | PASS | source report completed PASS |
| restore normal sync | INFO | set RESTORE_NORMAL=1 to restore certification service |

Overall status: PASS
