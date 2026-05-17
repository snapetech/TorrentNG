#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT"

run_gate() {
  local name="$1"
  shift
  printf '\n== %s ==\n' "$name"
  "$@"
}

run_gate "format" cargo fmt --check
run_gate "owned-read adoption guard" bash -c '
  offenders="$(rg -n "scheduled_read\\(" crates --glob "*.rs" \
    | grep -v "crates/rt-storage/src/scheduler.rs" || true)"
  if [[ -n "$offenders" ]]; then
    printf "%s\n" "$offenders"
    echo "production code must use scheduled_read_owned/read_owned_at unless deliberately extending the compatibility wrapper" >&2
    exit 1
  fi
'
run_gate "storage unit matrix" cargo test -p rt-storage
run_gate "storage certification script self-test" "$ROOT/scripts/storage_certification_selftest.sh"
run_gate "resource governor and scale proxies" cargo test -p rt-metrics
run_gate "configuration defaults" cargo test -p rt-config
run_gate "engine storage/resource consumers" cargo test -p rt-engine
run_gate "native API metrics projection" cargo test -p rt-api-native

if [[ "${STORAGE_NG_REAL_DEVICE:-0}" == "1" ]]; then
  run_gate "real-device storage probes" \
    bash -c 'STORAGE_PHASE_B_REAL_DEVICE=1 "$1"' _ "$ROOT/scripts/storage_phase_b_matrix.sh"
fi
