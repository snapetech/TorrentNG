#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
BIN="${TNG_RELEASE_BINARY:-$ROOT/target/release/torrentngd}"
CONFIG="${TNG_RELEASE_CONFIG:-$REPORT_DIR/backend-burndown-native-config-20260902.toml}"
STATIC_DIR="${TNG_STATIC_DIR:-$ROOT/sidecar/static}"
TOKEN="${TNG_RELEASE_TOKEN:-backend-burndown-token-20260902}"
OUT="${1:-$REPORT_DIR/backend-burndown-native-release-smoke-$(date -u +%Y%m%dT%H%M%SZ).md}"
PREFIX="${OUT%.md}"

mkdir -p "$(dirname "$OUT")"
test -x "$BIN"
test -f "$CONFIG"

LOG="${PREFIX}.log"
HEALTH="${PREFIX}.health.json"
TORRENTS="${PREFIX}.torrents.json"
TRANSFER="${PREFIX}.transfer.json"
QBIT="${PREFIX}.qbit.json"
QBIT_TRANSFER="${PREFIX}.qbit-transfer.json"
METRICS="${PREFIX}.metrics.txt"

daemon_pid=""
cleanup() {
  if [[ -n "$daemon_pid" ]] && kill -0 "$daemon_pid" 2>/dev/null; then
    kill -TERM "$daemon_pid" 2>/dev/null || true
    wait "$daemon_pid" 2>/dev/null || true
  fi
}
trap cleanup EXIT

start_epoch_ms="$(date +%s%3N)"
TORRENTNGD_CONFIG="$CONFIG" TNG_STATIC_DIR="$STATIC_DIR" "$BIN" >"$LOG" 2>&1 &
daemon_pid=$!

ready=0
for _ in $(seq 1 60); do
  if curl -fsS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:28080/health >"$HEALTH" 2>/dev/null; then
    ready=1
    break
  fi
  sleep 0.2
done
if [[ "$ready" != "1" ]]; then
  echo "daemon did not become ready; see $LOG" >&2
  exit 1
fi

curl -fsS -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:28080/api/v1/torrents?limit=2' >"$TORRENTS"
curl -fsS -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:28080/api/v1/transfer/info >"$TRANSFER"
curl -fsS -H "Authorization: Bearer $TOKEN" \
  'http://127.0.0.1:28080/api/qb/v2/torrents/info?limit=2' >"$QBIT"
curl -fsS -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:28080/api/qb/v2/transfer/info >"$QBIT_TRANSFER"
curl -fsS -H "Authorization: Bearer $TOKEN" \
  http://127.0.0.1:28080/metrics >"$METRICS"

jq -e '.ready == true and .engine.subsystems.engine.alive == true and .engine.subsystems.storage_workers.healthy == true' "$HEALTH" >/dev/null
jq -e 'has("snapshot") and has("total") and has("torrents")' "$TORRENTS" >/dev/null
jq -e 'has("dl_info_speed") and has("up_info_speed")' "$TRANSFER" >/dev/null
jq -e 'type == "array"' "$QBIT" >/dev/null
jq -e 'has("connection_status") and has("dl_info_speed")' "$QBIT_TRANSFER" >/dev/null
grep -q '^torrentng_storage_workers_healthy 1$' "$METRICS"
grep -q '^torrentng_api_snapshot_refreshes_total ' "$METRICS"

kill -TERM "$daemon_pid"
shutdown_polls=0
for _ in $(seq 1 50); do
  shutdown_polls=$((shutdown_polls + 1))
  if ! kill -0 "$daemon_pid" 2>/dev/null; then
    break
  fi
  sleep 0.2
done
wait "$daemon_pid"
daemon_pid=""

build_hash="$(sha256sum "$BIN" | awk '{print $1}')"
binary_bytes="$(stat -c '%s' "$BIN")"
metrics_lines="$(wc -l <"$METRICS" | tr -d ' ')"
metrics_bytes="$(wc -c <"$METRICS" | tr -d ' ')"
duration_ms="$(( $(date +%s%3N) - start_epoch_ms ))"

if git -C "$ROOT" diff --quiet --ignore-submodules -- && git -C "$ROOT" diff --cached --quiet --ignore-submodules --; then
  worktree="clean"
else
  worktree="dirty"
fi

{
  echo "# TorrentNG Native Release-Binary Smoke"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Binary: $BIN"
  echo "- Binary bytes: $binary_bytes"
  echo "- Binary SHA-256: $build_hash"
  echo "- Config: $CONFIG"
  echo "- Worktree: $worktree"
  echo
  echo "This run exercised the optimized production daemon binary with the"
  echo "authenticated native and qBittorrent facades, then sent SIGTERM and"
  echo "waited for process exit. It is a deployment smoke test, not 100k-scale"
  echo "capacity evidence."
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Evidence |"
  echo "|---|---|---|"
  echo "| Startup and health | PASS | $(basename "$HEALTH") |"
  echo "| Native list envelope | PASS | $(basename "$TORRENTS") |"
  echo "| Native transfer info | PASS | $(basename "$TRANSFER") |"
  echo "| qBittorrent list | PASS | $(basename "$QBIT") |"
  echo "| qBittorrent transfer info | PASS | $(basename "$QBIT_TRANSFER") |"
  echo "| Prometheus metrics | PASS | $(basename "$METRICS"); $metrics_lines lines / $metrics_bytes bytes |"
  echo "| SIGTERM and clean exit | PASS | $(basename "$LOG"); $shutdown_polls polls |"
  echo
  echo "- Total smoke duration milliseconds: $duration_ms"
  echo
  echo "Overall status: PASS"
} >"$OUT"

echo "$OUT"
