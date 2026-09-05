# TorrentNG External Evidence Preflight

- Date UTC: 2026-09-05T19:33:25Z
- Commit: ec17d79
- Corpus directory: /home/keith/Documents/code/TorrentNG/testdata/migration-corpus
- Storage target: unset
- Strict mode: 0

## Checks

| Check | Result | Detail |
|---|---|---|
| Docker daemon | PASS | reachable |
| public torrent opt-in | PASS | UNIVERSAL_LIVE_PUBLIC=1 |
| real-device storage target | WARN | set TNG_STORAGE_BENCH_DIR to a writable target mount |
| migration corpus coverage | PASS | all source-family directories contain migration evidence files |
| migration corpus manifest | PASS | validated by /home/keith/Documents/code/TorrentNG/certification/reports/external-preflight-migration-corpus-20260905T193325Z.md |
| 24h soak active | PASS | 2230423 /bin/bash /home/keith/Documents/code/TorrentNG/scripts/soak_certification.sh /home/keith/Documents/code/TorrentNG/.run/soak-24h-public-debian-20260905-v3.md |

Overall status: PASS_WITH_WARNINGS
Warnings: 1
