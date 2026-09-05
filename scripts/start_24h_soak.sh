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
USE_SYSTEMD="${TNG_24H_SOAK_USE_SYSTEMD:-1}"
SYSTEMD_UNIT="${TNG_24H_SOAK_SYSTEMD_UNIT:-torrentng-soak-24h-$STAMP}"
SYSTEMD_UNIT="${SYSTEMD_UNIT%.service}"

mkdir -p "$REPORT_DIR" "$RUN_DIR"

if pgrep -af '[s]oak_certification.sh' | grep -q 'soak-24h-'; then
  echo "24h soak already appears to be running:" >&2
  pgrep -af '[s]oak_certification.sh' | grep 'soak-24h-' >&2
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

duration="${SOAK_DURATION_SECONDS:-86400}"
interval="${SOAK_INTERVAL_SECONDS:-60}"
pid=""
supervisor="nohup"

if [[ "$USE_SYSTEMD" == "1" ]] && command -v systemd-run >/dev/null 2>&1 &&
  command -v systemctl >/dev/null 2>&1 && systemctl --user show-environment >/dev/null 2>&1; then
  systemd_env=(
    "--setenv=SOAK_DURATION_SECONDS=$duration"
    "--setenv=SOAK_INTERVAL_SECONDS=$interval"
  )
  for name in TNG_HOST_URL TNG_API_TOKEN TNG_CONTAINER SOAK_MAX_RSS_MB SOAK_LIST_LIMIT \
    SOAK_MAX_FDS SOAK_MAX_THREADS SOAK_MIN_DISK_FREE_MB SOAK_DATA_PATH \
    SOAK_EXPECTED_TORRENT_NAME SOAK_EXPECTED_TORRENT_HASH; do
    if [[ -n "${!name+x}" ]]; then
      systemd_env+=("--setenv=$name=${!name}")
    fi
  done
  systemd-run --user --unit="$SYSTEMD_UNIT" --collect --no-block \
    --property=Restart=on-failure \
    --property=RestartSec=5s \
    --property="StandardOutput=append:$LOG" \
    --property="StandardError=append:$LOG" \
    "${systemd_env[@]}" \
    /bin/bash "$ROOT/scripts/soak_certification.sh" "$OUT" >/dev/null
  for _ in $(seq 1 20); do
    pid="$(systemctl --user show "$SYSTEMD_UNIT.service" -p MainPID --value 2>/dev/null || true)"
    [[ "$pid" =~ ^[1-9][0-9]*$ ]] && break
    sleep 0.1
  done
  if [[ ! "$pid" =~ ^[1-9][0-9]*$ ]]; then
    systemctl --user stop "$SYSTEMD_UNIT.service" >/dev/null 2>&1 || true
    echo "systemd user unit did not expose a running soak process: $SYSTEMD_UNIT" >&2
    exit 1
  fi
  supervisor="systemd user unit $SYSTEMD_UNIT.service"
else
  if command -v setsid >/dev/null 2>&1; then
    SOAK_DURATION_SECONDS="$duration" SOAK_INTERVAL_SECONDS="$interval" \
      nohup setsid "$ROOT/scripts/soak_certification.sh" "$OUT" >"$LOG" 2>&1 &
  else
    SOAK_DURATION_SECONDS="$duration" SOAK_INTERVAL_SECONDS="$interval" \
      nohup "$ROOT/scripts/soak_certification.sh" "$OUT" >"$LOG" 2>&1 &
  fi
  pid="$!"
fi
printf '%s\n' "$pid" >"$PID_FILE"

echo "Started 24h soak"
echo "- PID: $pid"
echo "- Supervisor: $supervisor"
echo "- Report: $OUT"
echo "- Log: $LOG"
echo "- PID file: $PID_FILE"
