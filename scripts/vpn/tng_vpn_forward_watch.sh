#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ADAPTER="${TNG_VPN_ADAPTER:-$ROOT/scripts/vpn/tng_forward_from_vpn_state.sh}"
OUT_ENV="${TNG_VPN_OUT_ENV:-$ROOT/certification/reports/tng-vpn-forward.env}"
STATE_FILE="${TNG_VPN_WATCH_STATE:-$ROOT/certification/reports/tng-vpn-forward.watch}"
LOG_FILE="${TNG_VPN_WATCH_LOG:-$ROOT/certification/reports/tng-vpn-forward.watch.log}"
INTERVAL="${TNG_VPN_WATCH_INTERVAL:-30}"
MISS_LIMIT="${TNG_VPN_MISS_LIMIT:-0}"
RUN_DHT_CERT="${TNG_VPN_RUN_DHT_CERT:-1}"
RESTART_CMD="${TNG_VPN_RESTART_CMD:-restart-cert}"
ON_MISSING="${TNG_VPN_ON_MISSING:-mark}"

usage() {
  cat <<EOF
Usage: $(basename "$0") [--once]

Continuously watches slskdN-vpn-agent/Gluetun forwarded-port state and applies it
to TorrentNG when it appears or changes.

Important env:
  TNG_VPN_WATCH_INTERVAL   poll seconds, default 30
  TNG_VPN_MISS_LIMIT       consecutive misses before degraded action, 0 disables
  TNG_VPN_ON_MISSING       mark|stop-cert, default mark
  TNG_VPN_RUN_DHT_CERT     1 to run scripts/dht_certification.sh after changes
  TNG_VPN_RESTART_CMD      adapter command, default restart-cert

The adapter consumes the same env as tng_forward_from_vpn_state.sh.
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
TNG_VPN_FORWARD_STATUS=missing
TNG_VPN_FORWARD_LAST_MISS=$(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF
}

stop_cert() {
  local env_file="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
  local compose_file="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"
  docker compose --env-file "$env_file" -f "$compose_file" stop torrentng
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
    TNG_INCOMING_PORT="${TNG_INCOMING_PORT:-${TNG_VPN_PUBLIC_PORT:-50000}}" \
    TNG_VPN_PUBLIC_PORT="${TNG_VPN_PUBLIC_PORT:-}" \
    TNG_VPN_PUBLIC_IP="${TNG_VPN_PUBLIC_IP:-}" \
      "$ROOT/scripts/dht_certification.sh" || log "DHT certification failed after forward change"
  fi
}

run_once() {
  if mapping="$("$ADAPTER" print 2>/tmp/tng-vpn-watch.err)"; then
    apply_mapping "$mapping"
    return 0
  fi

  log "no forwarded port: $(tr '\n' ' ' </tmp/tng-vpn-watch.err)"
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
          log "stopping torrentng certification service because forward is missing"
          stop_cert
          ;;
        *)
          log "unknown TNG_VPN_ON_MISSING=$ON_MISSING"
          ;;
      esac
      misses=0
    fi
  fi
  sleep "$INTERVAL"
done
