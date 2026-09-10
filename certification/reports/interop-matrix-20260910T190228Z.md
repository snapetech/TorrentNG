# Interop Matrix Report

- Generated: 20260910T190228Z
- Mode: local
- Compose file: `deploy/interop/compose.yml`
- Workdir: `/home/keith/Documents/code/TorrentNG/certification/interop`
- Public timeout: 7200s
- Local timeout: 900s

# Local Swarm

## Local: rust-pulls-from-qbit

- Seeder: qbittorrent
- Leecher: torrentngd
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/rust-pulls-from-qbit.torrent`
- Status: **PASS**

## Local: rust-pulls-from-transmission

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/rust-pulls-from-transmission.torrent`
- Status: **PASS**

## Local: rust-pulls-from-deluge

- Seeder: deluge
- Leecher: torrentngd
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/rust-pulls-from-deluge.torrent`
- Status: **PASS**

## Local: rust-pulls-from-rtorrent

- Seeder: rtorrent
- Leecher: torrentngd
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/rust-pulls-from-rtorrent.torrent`
- Status: **PASS**

## Local: qbit-pulls-from-rust

- Seeder: torrentngd
- Leecher: qbittorrent
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/qbit-pulls-from-rust.torrent`
- Status: **PASS**

## Local: transmission-pulls-from-rust

- Seeder: torrentngd
- Leecher: transmission
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/transmission-pulls-from-rust.torrent`
- Status: **PASS**

## Local: deluge-pulls-from-rust

- Seeder: torrentngd
- Leecher: deluge
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/deluge-pulls-from-rust.torrent`
- Status: **PASS**

## Local: rtorrent-pulls-from-rust

- Seeder: torrentngd
- Leecher: rtorrent
- Fixture: single-16m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/rtorrent-pulls-from-rust.torrent`
- Status: **PASS**

## Local: mesh-swarm

- Seeder: all
- Leecher: all
- Fixture: multi-128m
- Torrent: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents/multi-128m.torrent`
- Status: **PASS**

## Local: churn

- Fixture count: 25
- Status: **PASS**

# Extended Local Certification

## Extended Local: rust-webseed-only

- Seeder: fixture-http
- Leecher: torrentngd
- Fixture: single-16m
- Torrent mode: webseed-only
- Status: **PASS**

## Extended Local: rust-explicit-peer-private

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-16m
- Torrent mode: private explicit peer, no tracker, no webseed
- Info hash: 3faaba52e0b1cbee64010dfa65dd8c09028e244c
- Status: **PASS**

## Extended Local: rust-restart-recovery

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-64m
- Restart delay: 5s
- Info hash: 8e41cbee166ef22b8e43468267373aa334f54ca2
- Status: **PASS**

## Extended Local: rust-api-facades

- Native REST: checked
- qBittorrent API: checked
- Transmission RPC facade: checked
- Deluge JSON-RPC facade: checked
- Metrics: checked
- Status: **PASS**

# Protocol Local Certification

## Protocol Local: rust-magnet-with-tracker

- Seeder: qbittorrent
- Leecher: torrentngd
- Fixture: single-16m
- Add method: magnet URL with HTTP tracker
- Info hash: ee8ebd1e12b7f30c64b3c19cff1137a4a9c29676
- Status: **PASS**

## Protocol Local: rust-trackerless-magnet

- Seeder: qbittorrent
- Leecher: torrentngd
- Fixture: single-16m
- Add method: trackerless magnet plus explicit peer bridge
- Info hash: a1a886f1d069433e0655e95d2d983afb49af1e98
- Status: **PASS**

## Protocol Local: rust-udp-tracker

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-16m
- Tracker: udp://opentracker:6969/announce
- Info hash: 945784de2851a0eac1e294c13645c1dc6843bebe
- Status: **PASS**

## Protocol Local: rust-multi-tracker-fallback

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-16m
- First tracker: http://127.0.0.1:9/dead-announce
- Fallback tracker: http://opentracker:6969/announce
- Info hash: 9e4857fbf9bd1c8b60a8364dc9e82fcfad484c06
- Status: **PASS**

## Protocol Local: tracker-outage-after-peer-discovery

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-64m
- Tracker stopped after: 3s
- Peer source after outage: explicit known peer
- Info hash: 53ce4647cbc98f97c1b73b5023d9c9ff23808cd2
- Status: **PASS**

## Protocol Local: webseed-outage-fallback

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-64m
- Webseed stopped after: 2s
- Fallback source: tracker/explicit known peer
- Info hash: fc23a3fd2f6c61e65543f76f5bdfa6c3b108261e
- Status: **PASS**

## Protocol Local: private-torrent-no-dht-pex

- Seeder: transmission
- Leecher: torrentngd
- Fixture: single-16m
- Torrent mode: private explicit peer, no tracker, no webseed
- DHT tracked torrents before add: 11
- DHT tracked torrents after private add: 11
- PEX policy: private torrents do not advertise ut_pex
- Info hash: 71a3e9466ae6a1fd49c782d7b95fb0c8be754981
- Status: **PASS**

## Protocol Local: resume-after-partial-download

- Seeder: fixture-http webseed
- Leecher: torrentngd
- Fixture: single-16m
- Preseeded partial bytes before add: 2097152
- Restart delay after add: 2s
- Facade progress after restart: 0.1455078125
- Info hash: 36a53ebaec70fc852df3cade1628e75e79120239
- Status: **PASS**

## Protocol Local: force-recheck-corruption-repair

- Seeder: fixture-http webseed
- Leecher: torrentngd
- Fixture: single-16m
- Corrupted bytes before recheck: 4096
- Info hash: bf4b57b775266d345181bfc23187cf83e0ec7171
- Status: **PASS**

## Protocol Local: missing-file-recovery

- Seeder: fixture-http webseed
- Leecher: torrentngd
- Fixture: single-16m
- Deleted before recheck: payload.bin
- Info hash: efbbc3b8ac3b1c859190c02fe491525dff25f062
- Status: **PASS**

## Protocol Local: rust-seeds-to-all-reference-clients

- Seeder: torrentngd
- Leechers: qbittorrent transmission deluge rtorrent
- Fixture: single-16m
- Info hash: 9ed9670b19929f1cf159796055941f9f4beec6eb
- Status: **PASS**

## Protocol Local: endgame-multi-peer

- Seeders: qbittorrent transmission deluge rtorrent
- Leecher: torrentngd
- Fixture: single-64m
- Torrent mode: tracker-only with all reference seeders bridged
- Info hash: 27f2c3786b7500c56f07d27a3695e36bbca85702
- Status: **PASS**

## Protocol Local: rust-partial-file-selection

- Seeder: transmission
- Leecher: torrentngd
- Fixture: multi-128m
- Torrent mode: private explicit peer, file 0 skipped
- Wanted files: part-1.bin through part-7.bin
- Skipped file: part-0.bin absent or empty
- Info hash: a679e8c52f367c93af6a70fb26c1960e05e7986b
- Status: **PASS**

## Protocol Local: rust-qbit-mutation-facade

- Target: torrentngd qBittorrent-compatible mutation endpoints
- Checked qBit facade: filePrio, torrent setDownloadLimit/setUploadLimit, transfer setDownloadLimit/setUploadLimit/toggleSpeedLimitsMode, setSuperSeeding, toggleFirstLastPiecePrio, addPeers, topPrio, recheck, addTrackers, editTracker, removeTrackers, trackers, files, webseeds, pieceStates, pieceHashes, export; setForceStart/setAutoTMM/setAutoManagement explicitly return 501
- Checked native REST: start, stop, update metadata, file priorities, tags, trackers, peers, queue, torrent limits, transfer limits, files/trackers/limits projection
- Fixture: multi-128m
- Info hash: 454aa97a4139f7794d3f2affbb43fe910fbc69d2
- Status: **PASS**

# Artifacts

- Logs: `/home/keith/Documents/code/TorrentNG/certification/interop/logs/20260910T190228Z`
- Torrents: `/home/keith/Documents/code/TorrentNG/certification/interop/torrents`
- API poll log: `/home/keith/Documents/code/TorrentNG/certification/interop/artifacts/rust-api-poll-20260910T190228Z.jsonl`

**Overall: PASS**
