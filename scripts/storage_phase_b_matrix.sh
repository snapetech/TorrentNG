#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

run() {
  printf '\n==> %s\n' "$*"
  "$@"
}

run cargo test -p rt-storage
run cargo test -p rt-storage device::tests
run cargo test -p rt-storage elevator::tests
run cargo test -p rt-storage auto_preallocation_policy
run cargo test -p rt-metrics storage_peer_read_readahead_reduces_backend_reads_for_adjacent_blocks -- --nocapture
run cargo test -p rt-metrics storage_hash_pool_does_not_block_peer_read_path -- --nocapture
run cargo test -p rt-metrics storage_positioned_io_preserves_offsets_under_concurrency -- --nocapture
run cargo test -p rt-metrics storage_file_pool_stays_bounded_under_active_file_churn -- --nocapture
run cargo test -p rt-engine upload_block_reads_across_many_file_regions
run cargo test -p rt-engine pure_v2_recheck_verifies_file_roots_without_torrent_task

if [[ "${STORAGE_PHASE_B_FULL:-0}" == "1" ]]; then
  run cargo test -p rt-engine
  run cargo test -p rt-metrics storage_ -- --nocapture
  run cargo test -p rt-api-qbit
  run cargo test -p rt-api-deluge
  run cargo test -p rt-migrate
fi

if [[ "${STORAGE_PHASE_B_REAL_DEVICE:-0}" == "1" ]]; then
  run cargo test -p rt-storage --test storage_real_device -- --ignored --nocapture
fi
