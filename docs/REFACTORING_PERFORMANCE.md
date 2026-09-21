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

Current release artifacts built from this worktree are:

- native daemon: 26,831,584 bytes
- sidecar binary: 16,008,272 bytes

The root release profile already uses `opt-level = 3`, thin LTO, one codegen
unit, and stripped debuginfo. The sidecar uses full LTO and symbol stripping.
No compiler-profile change was made without a before/after product benchmark.

## Dependency audit

The root graph still carries duplicate versions of `base64`, `tower`,
`tower-http`, and several crypto/random support crates. Some duplicates are
required by the axum/reqwest generations or are dev-only/transitive. They
should not be unified blindly; a future dependency cleanup should measure the
release daemon and sidecar artifacts independently.

## Verification

The affected slices passed:

- `rt-peer-wire`: 52 passing tests, 1 ignored microbenchmark
- `rt-engine`: 460 passing tests
- `rt-api-qbit`: 92 passing tests
- corrected sidecar release benchmarks at all four corpus sizes
- `cargo fmt --all -- --check`

The remaining production-scale memory and real-device storage claims stay
with the existing soak and hardware evidence gates.
