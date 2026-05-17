#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/universal-compat-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status="PASS"

mark_gate() {
  local name="$1"
  local result="$2"
  printf '| %s | %s |\n' "$name" "$result" >> "$OUT.table"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

run_gate() {
  local name="$1"
  shift
  {
    echo
    echo "## $name"
    echo
    echo '```text'
  } >> "$OUT"
  if (cd "$ROOT" && "$@") >> "$OUT" 2>&1; then
    echo '```' >> "$OUT"
    mark_gate "$name" "PASS"
  else
    echo '```' >> "$OUT"
    mark_gate "$name" "FAIL"
  fi
}

skip_gate() {
  local name="$1"
  local reason="$2"
  {
    echo
    echo "## $name"
    echo
    echo '```text'
    echo "SKIP: $reason"
    echo '```'
  } >> "$OUT"
  mark_gate "$name" "SKIP"
}

{
  echo "# TorrentNG Universal Compatibility Certification Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Rust: $(rustc --version 2>/dev/null || echo unavailable)"
  echo "- Cargo: $(cargo --version 2>/dev/null || echo unavailable)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result |"
  echo "|---|---|"
} > "$OUT"
: > "$OUT.table"

run_gate "API facade endpoint and field matrices" "$ROOT/scripts/api_facade_certification.sh" "$ROOT/certification/reports/api-facades-universal-$(date -u +%Y%m%dT%H%M%SZ).md"
run_gate "migration dry-run, DB import, and fastresume matrices" cargo test -p rt-migrate
run_gate "Track 1 sidecar qBittorrent compatibility flows" bash -c 'cd sidecar && cargo test qb_'
run_gate "native API compatibility manifest" cargo test -p rt-api-native
run_gate "native engine state, tracker, and storage hooks" cargo test -p rt-engine
run_gate "scale and metrics compatibility evidence" cargo test -p rt-metrics
run_gate "storage topology and peer-read matrix" "$ROOT/scripts/storage_phase_b_matrix.sh"

if [[ "${UNIVERSAL_COMPAT_LIVE:-0}" == "1" ]]; then
  run_gate "Docker client interop local matrix" "$ROOT/scripts/interop_matrix.sh" --local
else
  skip_gate "Docker client interop local matrix" "set UNIVERSAL_COMPAT_LIVE=1 to run Docker qBit/Transmission/Deluge/rTorrent transfer interop"
fi

if [[ "${UNIVERSAL_COMPAT_PUBLIC:-0}" == "1" ]]; then
  run_gate "public torrent interop matrix" "$ROOT/scripts/interop_matrix.sh" --public
else
  skip_gate "public torrent interop matrix" "set UNIVERSAL_COMPAT_PUBLIC=1 to download official public Linux torrents"
fi

if [[ "${UNIVERSAL_COMPAT_REAL_DEVICE:-0}" == "1" ]]; then
  run_gate "real-device storage matrix" bash -c 'STORAGE_PHASE_B_REAL_DEVICE=1 "$1"' _ "$ROOT/scripts/storage_phase_b_matrix.sh"
else
  skip_gate "real-device storage matrix" "set UNIVERSAL_COMPAT_REAL_DEVICE=1 and configure storage test paths to run ignored device tests"
fi

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
