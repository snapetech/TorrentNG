#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${1:-$REPORT_DIR/universal-live-$STAMP.md}"

RUN_LOCAL="${UNIVERSAL_LIVE_LOCAL:-1}"
RUN_PUBLIC="${UNIVERSAL_LIVE_PUBLIC:-0}"
RUN_REAL_DEVICE="${UNIVERSAL_LIVE_REAL_DEVICE:-0}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
skips=0
docker_ready=1

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >>"$OUT.table"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

have_cmd() {
  command -v "$1" >/dev/null 2>&1
}

run_gate() {
  local name="$1"
  shift
  {
    echo
    echo "## $name"
    echo
    echo "- Command: \`$*\`"
    echo
    echo '```text'
  } >>"$OUT"
  if (cd "$ROOT" && "$@") >>"$OUT" 2>&1; then
    echo '```' >>"$OUT"
    mark "$name" "PASS" "completed"
  else
    echo '```' >>"$OUT"
    mark "$name" "FAIL" "see report section"
  fi
}

skip_gate() {
  local name="$1"
  local reason="$2"
  skips=$((skips + 1))
  {
    echo
    echo "## $name"
    echo
    echo '```text'
    echo "SKIP: $reason"
    echo '```'
  } >>"$OUT"
  mark "$name" "SKIP" "$reason"
}

{
  echo "# TorrentNG Universal Live Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Run local Docker interop: $RUN_LOCAL"
  echo "- Run public torrent interop: $RUN_PUBLIC"
  echo "- Run real-device storage matrix: $RUN_REAL_DEVICE"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result | Detail |"
  echo "|---|---|---|"
} >"$OUT"
: >"$OUT.table"

if [[ "$RUN_LOCAL" == "1" || "$RUN_PUBLIC" == "1" ]]; then
  if ! have_cmd docker; then
    mark "Docker availability" "FAIL" "docker command missing"
    docker_ready=0
  elif ! docker info >/dev/null 2>&1; then
    mark "Docker availability" "FAIL" "docker daemon unavailable to current user"
    docker_ready=0
  else
    mark "Docker availability" "PASS" "docker daemon reachable"
  fi
fi

if [[ "$RUN_LOCAL" == "1" ]]; then
  if [[ "$docker_ready" == "1" ]]; then
    run_gate "Docker client interop local matrix" "$ROOT/scripts/interop_matrix.sh" --local
  else
    skip_gate "Docker client interop local matrix" "Docker preflight failed"
  fi
else
  skip_gate "Docker client interop local matrix" "set UNIVERSAL_LIVE_LOCAL=1"
fi

if [[ "$RUN_PUBLIC" == "1" ]]; then
  if [[ "$docker_ready" == "1" ]]; then
    run_gate "public torrent interop matrix" "$ROOT/scripts/interop_matrix.sh" --public
  else
    skip_gate "public torrent interop matrix" "Docker preflight failed"
  fi
else
  skip_gate "public torrent interop matrix" "set UNIVERSAL_LIVE_PUBLIC=1 after approving public legal torrent downloads"
fi

if [[ "$RUN_REAL_DEVICE" == "1" ]]; then
  run_gate "real-device storage matrix" bash -c 'STORAGE_PHASE_B_REAL_DEVICE=1 "$1"' _ "$ROOT/scripts/storage_phase_b_matrix.sh"
else
  skip_gate "real-device storage matrix" "set UNIVERSAL_LIVE_REAL_DEVICE=1 and configure TNG_STORAGE_BENCH_DIR for target hardware"
fi

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  if [[ "$status" == "PASS" && "$skips" -gt 0 ]]; then
    echo "Overall status: PASS_WITH_SKIPS"
    echo "Skipped gates: $skips"
  else
    echo "Overall status: $status"
  fi
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
