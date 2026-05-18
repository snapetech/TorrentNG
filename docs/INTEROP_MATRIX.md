# Docker Interop Matrix

The Docker interop matrix is the release certification harness for proving that
`torrentngd` works with common BitTorrent clients and with real legal public
swarms. It runs the native daemon beside qBittorrent, Transmission, Deluge,
rTorrent, opentracker, and a fixture HTTP server.

The runner is:

```sh
scripts/interop_matrix.sh
```

The Compose stack is:

```text
deploy/interop/compose.yml
```

Public torrent sources are configured in:

```text
deploy/interop/public-torrents.toml
```

## Scope

The matrix has two legs.

| Leg | Purpose | Default gate |
|---|---|---|
| Local deterministic swarm | Strict client-to-client behavior using generated legal fixture torrents and a local opentracker instance. | Required fast gate |
| Public legal torrents | Real swarm behavior using official Debian, Ubuntu, and Fedora torrents resolved at runtime. | Required release gate |

LibreOffice is available as optional desktop-application coverage when its
official torrent resolver matches an available release.

## Run

Run the local deterministic matrix:

```sh
scripts/interop_matrix.sh --local
```

Run the public legal torrent matrix:

```sh
scripts/interop_matrix.sh --public
```

Run both:

```sh
scripts/interop_matrix.sh --all
```

Write a report to a known path:

```sh
scripts/interop_matrix.sh --local --report certification/reports/interop-local.md
```

The script builds and starts the interop Compose stack by default. Use
`INTEROP_SKIP_BUILD=1` when the `torrentng/torrentngd:interop` image is
already current.

```sh
INTEROP_SKIP_BUILD=1 scripts/interop_matrix.sh --local
```

## Local Matrix

Local mode generates legal fixture torrents under
`certification/interop/fixtures`, seeds them from one client, downloads them
from another, and verifies payload hashes on disk.

| Case | Seeder | Leecher | Fixture | Pass criteria |
|---|---|---|---|---|
| `rust-pulls-from-qbit` | qBittorrent | Rust | single-file 16 MiB | Complete and hash match |
| `rust-pulls-from-transmission` | Transmission | Rust | single-file 16 MiB | Complete and hash match |
| `rust-pulls-from-deluge` | Deluge | Rust | single-file 16 MiB | Complete and hash match |
| `rust-pulls-from-rtorrent` | rTorrent | Rust | single-file 16 MiB | Complete and hash match |
| `qbit-pulls-from-rust` | Rust | qBittorrent | single-file 16 MiB | Complete and hash match |
| `transmission-pulls-from-rust` | Rust | Transmission | single-file 16 MiB | Complete and hash match |
| `deluge-pulls-from-rust` | Rust | Deluge | single-file 16 MiB | Complete and hash match |
| `rtorrent-pulls-from-rust` | Rust | rTorrent | single-file 16 MiB | Complete and hash match |
| `mesh-swarm` | All clients | All clients | multi-file 128 MiB | All complete and all hashes match |
| `churn` | Rotating clients | Rotating clients | 25 small torrents | No client error and Rust remains healthy |

Extended local coverage is enabled by default with `INTEROP_EXTENDED_LOCAL=1`.

| Case | Coverage | Pass criteria |
|---|---|---|
| `rust-webseed-only` | Webseed-only torrent with no peer availability counts. | Rust completes from fixture HTTP and the hash matches |
| `rust-explicit-peer-private` | Private trackerless torrent with an explicit Transmission peer. | Rust completes after explicit peer injection and the hash matches |
| `rust-restart-recovery` | Restart during an active download from Transmission. | Rust recovers, reconnects, completes, and verifies the hash |
| `rust-api-facades` | API health while transfers are active. | Native, qBit-compatible, Transmission facade, Deluge facade, health, and metrics endpoints return without 5xx failures |

Run only the extended local cases:

```sh
INTEROP_EXTENDED_ONLY=1 scripts/interop_matrix.sh --local
```

Disable the extended cases for a narrower legacy local run:

```sh
INTEROP_EXTENDED_LOCAL=0 scripts/interop_matrix.sh --local
```

## Protocol Matrix

Protocol local coverage is enabled by default with `INTEROP_PROTOCOL_LOCAL=1`.
These rows sit between the deterministic transfer matrix and the public swarm
matrix: they use local legal fixtures, but target specific BitTorrent protocol
or compatibility behaviors.

| Case | Coverage | Pass criteria | Status |
|---|---|---|---|
| `rust-magnet-with-tracker` | Rust adds a `btih` magnet URI with an HTTP tracker, fetches metadata from a reference seeder, and downloads the payload. | Complete and hash match | Implemented |
| `rust-udp-tracker` | Rust announces to opentracker through `udp://opentracker:6969/announce`. | Complete and hash match | Implemented |
| `rust-qbit-mutation-facade` | qBittorrent-compatible `filePrio`, `recheck`, tracker add/edit/remove, `trackers`, and `files` endpoints. | Endpoints succeed and reflected state is visible | Implemented |
| `magnet-dht-only` | Magnet metadata and peer discovery without trackers. DHT `get_peers` forwarding into torrent peer commands is covered by `rt-engine` tests; full local trackerless metadata fetch and transfer remains planned. | Complete and hash match | Planned |
| `rust-multi-tracker-fallback` | Dead tracker in the first tier, working tracker fallback. | Rust completes through fallback tracker | Implemented |
| `tracker-outage-after-peer-discovery` | Stop the local tracker after TorrentNG has an explicit known peer for a tracker-only transfer. | Transfer continues through the known peer and final hash matches | Implemented |
| `private-torrent-no-dht-pex` | Private torrent policy enforcement with no tracker and an explicit allowed peer. | DHT registration does not increase, PEX is not advertised, explicit peer transfer completes, and final hash matches | Implemented |
| `rust-partial-file-selection` | Multi-file priority and wanted/unwanted file behavior during transfer. | Wanted files complete; skipped file remains absent or empty | Implemented |
| `force-recheck-corruption-repair` | Complete from a local webseed, corrupt on-disk bytes, force recheck, and redownload the damaged range. | Corruption detected, repair completes, and final hash matches | Implemented |
| `resume-after-partial-download` | Start with a valid partial 16 MiB fixture on disk, restart Rust after add, and resume through local webseed availability. | Rust restarts, resumes, completes, and final hash matches | Implemented |
| `missing-file-recovery` | Complete from a local webseed, delete the payload file, force recheck, and redownload the missing file. | Missing file is detected, recreated, and final hash matches | Implemented |
| `endgame-multi-peer` | TorrentNG downloads a 64 MiB fixture while all four reference clients seed the same tracker-only torrent. | Completes from multiple peer sources without duplicate-write corruption or stalls, and final hash matches | Implemented |
| `rust-seeds-to-all-reference-clients` | Rust as the only long-running seeder for qBit, Transmission, Deluge, and rTorrent. | All reference clients complete from Rust and final hashes match | Implemented |

Disable protocol rows while debugging only the older deterministic cases:

```sh
INTEROP_PROTOCOL_LOCAL=0 scripts/interop_matrix.sh --local
```

Run one protocol row while developing it:

```sh
INTEROP_PROTOCOL_ONLY=rust-udp-tracker scripts/interop_matrix.sh --local
```

Run the magnet metadata row directly:

```sh
INTEROP_PROTOCOL_ONLY=rust-magnet-with-tracker scripts/interop_matrix.sh --local
```

Run the private torrent policy row directly:

```sh
INTEROP_PROTOCOL_ONLY=private-torrent-no-dht-pex scripts/interop_matrix.sh --local
```

Run the tracker outage row directly:

```sh
INTEROP_PROTOCOL_ONLY=tracker-outage-after-peer-discovery scripts/interop_matrix.sh --local
```

Run the partial-resume row directly:

```sh
INTEROP_PROTOCOL_ONLY=resume-after-partial-download scripts/interop_matrix.sh --local
```

Run the corruption-repair row directly:

```sh
INTEROP_PROTOCOL_ONLY=force-recheck-corruption-repair scripts/interop_matrix.sh --local
```

Run the missing-file recovery row directly:

```sh
INTEROP_PROTOCOL_ONLY=missing-file-recovery scripts/interop_matrix.sh --local
```

Run the Rust-to-all-reference-clients seeding row directly:

```sh
INTEROP_PROTOCOL_ONLY=rust-seeds-to-all-reference-clients scripts/interop_matrix.sh --local
```

Run the multi-peer completion row directly:

```sh
INTEROP_PROTOCOL_ONLY=endgame-multi-peer scripts/interop_matrix.sh --local
```

When `INTEROP_PROTOCOL_ONLY` is set, the runner skips the base local and
extended local cases and runs only the requested protocol row.

## Expansion Backlog

These rows are not required by the default gate yet. They define the remaining
coverage needed before claiming broad BitTorrent compatibility rather than
strong baseline interoperability.

| Area | Planned rows |
|---|---|
| Magnet links | `magnet-with-tracker`, `magnet-dht-only`, `magnet-metadata-from-qbit`, `magnet-metadata-from-transmission`, `magnet-resume-after-restart` |
| DHT, PEX, LSD | `dht-only-discovery`, `pex-peer-discovery`, `lsd-docker-lan-discovery`, `dht-bootstrap-recovery-after-restart` |
| Trackers | `http-tracker-announce-scrape`, `udp-tracker-announce-scrape`, `multi-tracker-tiers`, `private-tracker-policy` |
| Protocol behavior | `extension-handshake`, `ut-metadata`, `fast-extension`, `choke-unchoke-contention`, `optimistic-unchoke`, `endgame-mode`, `rarest-first-partial-availability` |
| File layouts | `single-file`, `deep-multi-file-tree`, `empty-files`, `unicode-paths`, `space-and-shell-hostile-paths`, `small-piece-size`, `large-piece-size` |
| State and recovery | `pause-resume-persistence`, `force-recheck`, `move-storage-path`, `delete-torrent-only`, `delete-with-data`, `resume-partial-files`, `corrupt-block-repair`, `missing-file-recovery` |
| API compatibility | `qbit-arr-endpoints`, `transmission-write-rpc`, `deluge-write-json-rpc`, `error-shape-compatibility`, `tracker-mutation-compatibility`, `file-priority-compatibility` |
| Performance and stress | `hundreds-small-torrents`, `many-peers-per-torrent`, `parallel-public-torrents`, `long-active-soak`, `memory-fd-growth`, `rate-limit-behavior` |
| Network adversity | `reference-client-restart`, `rust-restart-mid-transfer`, `webseed-outage-fallback`, `slow-peer`, `corrupt-peer`, `peer-disconnect-churn`, `ipv6-transfer`; tracker outage after peer discovery is implemented as `tracker-outage-after-peer-discovery` |
| Seeding | `rust-long-running-seeder`, `upload-accounting`, `ratio-seed-limit`, `time-seed-limit`, `multiple-leechers-from-rust`, `reference-clients-complete-from-rust-alone` |

## Public Matrix

Public mode resolves legal torrents from official project infrastructure at
runtime. The default enabled sources are:

| Source | Resolver | Clients |
|---|---|---|
| Debian | `https://cdimage.debian.org/debian-cd/current/amd64/bt-cd/` | Rust, qBittorrent, Transmission, Deluge, rTorrent |
| Ubuntu | `https://releases.ubuntu.com/` | Rust, qBittorrent, Transmission, Deluge, rTorrent |
| Fedora | `https://torrent.fedoraproject.org/torrents/` | Rust, qBittorrent, Transmission, Deluge, rTorrent |
| LibreOffice | `https://download.documentfoundation.org/libreoffice/stable/` | Rust and qBittorrent, optional |

Public pass criteria are intentionally practical for live swarms:

- All clients complete the download, or Rust completes and observes peers from
  at least two client families.
- Rust `/health` stays ready.
- Rust `/metrics` remains scrapeable.
- No required torrent enters a terminal error state.
- Resolver failures are reported separately from transfer failures.

Run one public source while debugging:

```sh
INTEROP_PUBLIC_ONLY=debian scripts/interop_matrix.sh --public
```

Enable LibreOffice:

```sh
INTEROP_INCLUDE_LIBREOFFICE=1 scripts/interop_matrix.sh --public
```

Keep public payload data after report generation:

```sh
INTEROP_KEEP_PUBLIC_DATA=1 scripts/interop_matrix.sh --public
```

## API Coverage

The matrix polls Rust through each supported API facade while transfers are
active:

| Surface | Endpoints or calls |
|---|---|
| Native | `/health`, `/metrics`, `/api/v1/torrents` |
| qBittorrent-compatible | `/api/qb/v2/torrents/info`, `/api/qb/v2/sync/maindata`, `/api/qb/v2/transfer/info` |
| Transmission RPC facade | `session-stats`, `torrent-get` |
| Deluge JSON-RPC facade | `web.update_ui`, `core.get_torrents_status` |

The API checks fail the row on 5xx responses, an unhealthy Rust daemon,
unscrapeable metrics, or terminal torrent errors.

## Reports And Artifacts

Reports are written under `certification/reports/` by default:

```text
certification/reports/interop-matrix-<timestamp>.md
```

Each matrix row records the case name, clients, torrent metadata, add method,
timing, completion status, peer observations, and file-hash result. On failure,
the runner captures the Compose state, recent service logs, Rust health,
metrics, and torrent API snapshots under:

```text
certification/interop/logs/<timestamp>/
```

Set `INTEROP_KEEP_STACK=1` to preserve containers after a failed run:

```sh
INTEROP_KEEP_STACK=1 scripts/interop_matrix.sh --local
```

## Environment

| Variable | Default | Meaning |
|---|---:|---|
| `INTEROP_LOCAL_TIMEOUT_SECS` | `900` | Per-local-row timeout |
| `INTEROP_PUBLIC_TIMEOUT_SECS` | `7200` | Per-public-source timeout |
| `INTEROP_PUBLIC_MAX_PARALLEL` | `3` | Maximum public torrents active at once |
| `INTEROP_PUBLIC_MIN_RUST_PEERS` | `2` | Minimum Rust peer observation threshold for public fallback pass criteria |
| `INTEROP_INCLUDE_LIBREOFFICE` | `0` | Include optional LibreOffice public source |
| `INTEROP_PUBLIC_ONLY` | unset | Run one public source, such as `debian`, `ubuntu`, or `fedora` |
| `INTEROP_EXTENDED_LOCAL` | `1` | Include extended local coverage |
| `INTEROP_EXTENDED_ONLY` | `0` | Run only the extended local rows |
| `INTEROP_PROTOCOL_LOCAL` | `1` | Include protocol-specific local coverage |
| `INTEROP_PROTOCOL_ONLY` | unset | Run one protocol row by case name |
| `INTEROP_SKIP_BUILD` | `0` | Reuse an existing interop image instead of building |
| `INTEROP_KEEP_STACK` | `0` | Leave containers running after the script exits |
| `INTEROP_KEEP_PUBLIC_DATA` | `0` | Preserve public torrent payloads after report generation |
| `INTEROP_CURL_MAX_TIME` | `10` | Per-control-plane curl timeout |
| `INTEROP_WORKDIR` | `certification/interop` | Matrix working directory |

Host ports can be overridden with the `INTEROP_*_HOST_PORT` and
`INTEROP_*_PEER_PORT` variables used by `deploy/interop/compose.yml`.

Default host ports avoid the common Linux ephemeral range:

| Service | Host port |
|---|---:|
| Rust API | `28180` |
| qBittorrent Web API | `28181` |
| Transmission RPC | `28191` |
| Deluge Web | `28212` |
| rTorrent peer | `29185` |
| opentracker | `26969` |
| fixture HTTP | `28188` |

## Release Gate

Use these gates before treating native-engine interop as release-ready:

```sh
cargo test --workspace
scripts/interop_matrix.sh --local
scripts/interop_matrix.sh --public
```

The local matrix is the strict deterministic source of truth for
client-to-client behavior. The public matrix proves live swarm behavior against
official legal torrents, but public swarm health can vary, so reports distinguish
resolver failures, transfer failures, and peer-observation fallback passes.

Passing this matrix is a strong interoperability signal. It does not mean every
BitTorrent extension or every tracker/client combination is complete.
