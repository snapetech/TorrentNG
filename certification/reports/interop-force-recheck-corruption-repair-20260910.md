# Interop Matrix Report

- Generated: 20260910T174200Z
- Mode: local
- Compose file: `deploy/interop/compose.yml`
- Workdir: `/home/keith/Documents/code/TorrentNG/certification/interop`
- Public timeout: 7200s
- Local timeout: 900s

# Local Swarm

- Base local cases skipped because INTEROP_PROTOCOL_ONLY=force-recheck-corruption-repair

# Protocol Local Certification

## Protocol Local: force-recheck-corruption-repair

- Seeder: fixture-http webseed
- Leecher: torrentngd
- Fixture: single-16m
- Corrupted bytes before recheck: 4096
- Info hash: 1bfc8681d690b72bdb8d76234538b84a05b6a7ef
- Status: **PASS**

# Artifacts

- Logs: `/home/keith/Documents/code/TorrentNG/certification/interop/logs/20260910T174200Z`
- Torrents: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents`
- API poll log: `/home/keith/Documents/code/TorrentNG/certification/interop/artifacts/rust-api-poll-20260910T174200Z.jsonl`

**Overall: PASS**
