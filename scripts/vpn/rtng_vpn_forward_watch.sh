#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ADAPTER="${RTNG_VPN_ADAPTER:-$ROOT/scripts/vpn/rtng_forward_from_vpn_state.sh}"
OUT_ENV="${RTNG_VPN_OUT_ENV:-$ROOT/certification/reports/rtng-vpn-forward.env}"
STATE_FILE="${RTNG_VPN_WATCH_STATE:-$ROOT/certification/reports/rtng-vpn-forward.watch}"
LOG_FILE="${RTNG_VPN_WATCH_LOG:-$ROOT/certification/reports/rtng-vpn-forward.watch.log}"
INTERVAL="${RTNG_VPN_WATCH_INTERVAL:-30}"
MISS_LIMIT="${RTNG_VPN_MISS_LIMIT:-0}"
RUN_DHT_CERT="${RTNG_VPN_RUN_DHT_CERT:-1}"
RESTART_CMD="${RTNG_VPN_RESTART_CMD:-restart-cert}"
ON_MISSING="${RTNG_VPN_ON_MISSING:-mark}"

usage() {
  cat <<EOF
Usage: $(basename "$0") [--once]

Continuously watches slskdN-vpn-agent/Gluetun forwarded-port state and applies it
to rtorrentNG when it appears or changes.

Important env:
  RTNG_VPN_WATCH_INTERVAL   poll seconds, default 30
  RTNG_VPN_MISS_LIMIT       consecutive misses before degraded action, 0 disables
  RTNG_VPN_ON_MISSING       mark|stop-cert, default mark
  RTNG_VPN_RUN_DHT_CERT     1 to run scripts/dht_certification.sh after changes
  RTNG_VPN_RESTART_CMD      adapter command, default restart-cert

The adapter consumes the same env as rtng_forward_from_vpn_state.sh.
EOF
}

log() {
  mkdir -p "$(dirname "$LOG_FILE")"
  printf '%s %s\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$*" | tee -a "$LOG_FILE"
}

fingerprint() {
  sed -n 's/^\(public_ip\|public_port\|private_port\|proto\|source\)=//p' | paste -sd '|'
}

mark_degraded() {
  mkdir -p "$(dirname "$OUT_ENV")"
  cat > "$OUT_ENV" <<EOF
RTNG_VPN_FORWARD_STATUS=missing
RTNG_VPN_FORWARD_LAST_MISS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
}

stop_cert() {
  local env_file="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
  local compose_file="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"
  docker compose --env-file "$env_file" -f "$compose_file" stop rtorrentng
}

apply_mapping() {
  local mapping="$1"
  local current_fp previous_fp
  current_fp="$(fingerprint <<<"$mapping")"
  previous_fp="$(cat "$STATE_FILE" 2>/dev/null || true)"
  if [[ "$current_fp" == "$previous_fp" ]]; then
    log "forward unchanged: $current_fp"
    return 0
  fi

  log "forward changed: ${previous_fp:-none} -> $current_fp"
  "$ADAPTER" "$RESTART_CMD"
  printf '%s' "$current_fp" > "$STATE_FILE"

  if [[ "$RUN_DHT_CERT" == "1" ]]; then
    set -a
    [[ -f "$OUT_ENV" ]] && source "$OUT_ENV"
    set +a
    RTNG_INCOMING_PORT="${RTNG_INCOMING_PORT:-${RTNG_VPN_PUBLIC_PORT:-50000}}" \
    RTNG_VPN_PUBLIC_PORT="${RTNG_VPN_PUBLIC_PORT:-}" \
    RTNG_VPN_PUBLIC_IP="${RTNG_VPN_PUBLIC_IP:-}" \
      "$ROOT/scripts/dht_certification.sh" || log "DHT certification failed after forward change"
  fi
}

run_once() {
  if mapping="$("$ADAPTER" print 2>/tmp/rtng-vpn-watch.err)"; then
    apply_mapping "$mapping"
    return 0
  fi

  log "no forwarded port: $(tr '\n' ' ' </tmp/rtng-vpn-watch.err)"
  mark_degraded
  return 1
}

[[ "${1:-}" == "-h" || "${1:-}" == "--help" ]] && { usage; exit 0; }

if [[ "${1:-}" == "--once" ]]; then
  run_once
  exit $?
fi

misses=0
while true; do
  if run_once; then
    misses=0
  else
    misses=$((misses + 1))
    if (( MISS_LIMIT > 0 && misses >= MISS_LIMIT )); then
      log "forward missing for $misses checks"
      case "$ON_MISSING" in
        mark)
          ;;
        stop-cert)
          log "stopping rtorrentng certification service because forward is missing"
          stop_cert
          ;;
        *)
          log "unknown RTNG_VPN_ON_MISSING=$ON_MISSING"
          ;;
      esac
      misses=0
    fi
  fi
  sleep "$INTERVAL"
done
