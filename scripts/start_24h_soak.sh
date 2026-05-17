#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
RUN_DIR="${TNG_RUN_DIR:-$ROOT/.run}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${1:-$REPORT_DIR/soak-24h-$STAMP.md}"
LOG="${TNG_24H_SOAK_LOG:-$RUN_DIR/soak-24h-$STAMP.log}"
PID_FILE="${TNG_24H_SOAK_PID_FILE:-$RUN_DIR/soak-24h.pid}"
DRY_RUN="${TNG_24H_SOAK_DRY_RUN:-0}"

mkdir -p "$REPORT_DIR" "$RUN_DIR"

if pgrep -af 'soak_certification.sh' | grep -q 'soak-24h-'; then
  echo "24h soak already appears to be running:" >&2
  pgrep -af 'soak_certification.sh' | grep 'soak-24h-' >&2
  exit 1
fi

if ! command -v docker >/dev/null 2>&1; then
  echo "docker command missing; soak_certification.sh requires container RSS sampling" >&2
  exit 1
fi

if ! docker info >/dev/null 2>&1; then
  echo "docker daemon unavailable to current user; soak_certification.sh requires container access" >&2
  exit 1
fi

if [[ "$DRY_RUN" == "1" ]]; then
  echo "24h soak dry run"
  echo "- Report: $OUT"
  echo "- Log: $LOG"
  echo "- PID file: $PID_FILE"
  echo "- Duration seconds: ${SOAK_DURATION_SECONDS:-86400}"
  echo "- Interval seconds: ${SOAK_INTERVAL_SECONDS:-60}"
  exit 0
fi

SOAK_DURATION_SECONDS="${SOAK_DURATION_SECONDS:-86400}" \
SOAK_INTERVAL_SECONDS="${SOAK_INTERVAL_SECONDS:-60}" \
nohup "$ROOT/scripts/soak_certification.sh" "$OUT" >"$LOG" 2>&1 &
pid="$!"
printf '%s\n' "$pid" >"$PID_FILE"

echo "Started 24h soak"
echo "- PID: $pid"
echo "- Report: $OUT"
echo "- Log: $LOG"
echo "- PID file: $PID_FILE"
