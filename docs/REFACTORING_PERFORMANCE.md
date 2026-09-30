# Refactoring and performance pass

Date: 2026-09-21

This pass keeps behavior changes bounded and records measurements separately
from external scale or soak evidence.

## Changes

- `rt-peer-wire::Message::encode_into` appends directly to a reusable
  `BytesMut`. `PeerCodec`, torrent uTP peers, and metadata uTP peers use the
  direct path. The uTP write buffer retains a resource-governor lease for its
  retained capacity.
- The qBittorrent `sync/maindata` projection moved to
  `crates/rt-api-qbit/src/sync_handlers.rs` without changing its public
  handler names or routes.
- Engine command and metainfo validation moved to
  `crates/rt-engine/src/engine_validation.rs`; the original private names are
  re-exported inside the engine module so call sites remain unchanged.
- The sidecar benchmark now uses the persisted logical revision cursor and
  respects the qBittorrent 5,000-row page limit. The previous benchmark
  produced false failures by using a wall-clock-incompatible cursor and by
  expecting an uncapped response.

## Measurements

The ignored sidecar benchmark was run in release mode with the following
corpus sizes. `torrents/info` returns a bounded 5,000-row page when the corpus
is larger than that.

| Corpus | Info page | Sync delta |
|---:|---:|---:|
| 1,000 | 5.31 ms | 1.94 ms |
| 10,000 | 29.17 ms | 2.40 ms |
| 15,000 | 29.17 ms | 2.80 ms |
| 50,000 | 33.72 ms | 4.70 ms |

The peer-wire release microbenchmark encoded 10,000 16 KiB Piece frames in
724.7 µs through an owned `Vec` and 618.5 µs through the reused direct buffer
(14.7% faster in that isolated loop). This is not an end-to-end transfer
throughput claim.

Release artifact sizes captured during the 2026-09-21 pass were:

- native daemon: 26,831,584 bytes
- sidecar binary: 16,008,272 bytes

The root release profile already uses `opt-level = 3`, thin LTO, one codegen
unit, and stripped debuginfo. The sidecar uses full LTO and symbol stripping.
No compiler-profile change was made without a before/after product benchmark.

## Dependency audit baseline (2026-09-21)

The initial audit found duplicate versions of `base64`, `tower`, `tower-http`,
and crypto/random support crates. The scoped follow-up below consolidated the
compatible direct dependencies and records the framework-constrained versions
that remain.

## Verification

The affected slices passed:

- `rt-peer-wire`: 52 passing tests, 1 ignored microbenchmark
- `rt-engine`: 460 passing tests
- `rt-api-qbit`: 92 passing tests
- corrected sidecar release benchmarks at all four corpus sizes
- `cargo fmt --all -- --check`

The remaining production-scale memory and real-device storage claims stay
with the existing soak and hardware evidence gates.


## Engine module and dependency cleanup (2026-09-25)

The engine source split is complete. `Engine` remains the ordering authority
for its state transitions and `TorrentTask` remains the authority for
per-torrent state. No actor mailbox, protocol ordering, or public API changed.
The extracted private modules are:

- `crates/rt-engine/src/engine/handle.rs` for the cloneable command facade.
- `crates/rt-engine/src/engine/lifecycle.rs` and
  `crates/rt-engine/src/engine/restore.rs` for torrent task lifecycle and
  startup/job recovery.
- `crates/rt-engine/src/engine/storage.rs` for move/delete/storage-plan
  choreography.
- `crates/rt-engine/src/engine/read_model.rs` for statistics, health, and
  diagnostics.
- `rt-engine/src/torrent_task/peer_connections.rs`, `peer_session.rs`, and
  `peer_transfer.rs` for peer selection, session transport, and block transfer.

The root workspace now uses `base64 0.22`, matching its Axum 0.7 and Reqwest
0.12 graph, instead of retaining a direct 0.23 copy. Root `sha1` is updated to
0.10.7, matching the sidecar lockfile patch version.

The remaining duplicate versions are dependency-generation boundaries visible
in `cargo tree --workspace -d` and `sidecar/cargo tree -d`:

- Root `tower 0.4` / `0.5` and `tower-http 0.5` / `0.6` come from the Axum 0.7
  and Reqwest 0.12 dependency generations.
- Root `rand 0.8` / `0.9` / `0.10` comes from Tungstenite, Proptest, and
  TorrentNG's direct use, respectively. Root `digest` and `crypto-common`
  0.10 / 0.11 follow the separate SHA-1 0.10 and SHA-2 0.11 APIs; `hashbrown`
  0.14 / 0.17 follows rusqlite and the TOML/indexmap stack.
- The separately locked sidecar uses Axum 0.8 and Reqwest 0.13, which require
  both `base64 0.22` and `0.23`; Reqwest's `tower-http 0.6` also coexists with
  the sidecar's direct `0.7` integration.

Unifying those remaining entries requires framework or test-tool version
changes, not a safe manifest alignment. The root `cargo tree` now has one
`base64` version. Locked offline checks and debug/release builds passed for the
root workspace; the locked offline release build passed for the sidecar.
