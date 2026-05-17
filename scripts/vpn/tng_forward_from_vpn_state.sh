#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
COMPOSE_FILE="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"

STATE_DIR="${TNG_VPN_STATE_DIR:-/var/lib/slskdN-vpn}"
STATIC_FORWARD_DIR="${TNG_VPN_STATIC_FORWARD_DIR:-/etc/slskdN-vpn/static-forwards}"
GLUETUN_API="${TNG_VPN_GLUETUN_API:-}"
PRIVATE_PORT="${TNG_VPN_PRIVATE_PORT:-50000}"
PROTO="${TNG_VPN_PROTO:-tcp}"
OUT_ENV="${TNG_VPN_OUT_ENV:-$ROOT/certification/reports/tng-vpn-forward.env}"

usage() {
  cat <<EOF
Usage: $(basename "$0") [print|write-env|restart-cert]

Reads slskdN-vpn-agent/Gluetun-style forwarded-port state and adapts it for
TorrentNG. Secrets stay outside git; this script only consumes runtime state.

Inputs:
  TNG_VPN_STATE_DIR              default /var/lib/slskdN-vpn
  TNG_VPN_STATIC_FORWARD_DIR     default /etc/slskdN-vpn/static-forwards
  TNG_VPN_GLUETUN_API            optional, e.g. http://127.0.0.1:8000
  TNG_VPN_PRIVATE_PORT           private target port, default 50000
  TNG_VPN_PROTO                  tcp or udp, default tcp
  TNG_VPN_OUT_ENV                output env file

Commands:
  print         print discovered mapping
  write-env     write TNG_INCOMING_PORT and TNG_VPN_PUBLIC_* env file
  restart-cert  write env and restart certification torrentng with forwarded port
EOF
}

read_env_file() {
  local file="$1"
  [[ -r "$file" ]] || return 1
  local local_port target_port public_port public_ip proto
  local_port="$(sed -n 's/^local_port=//p' "$file" | tail -1)"
  target_port="$(sed -n 's/^target_port=//p' "$file" | tail -1)"
  public_port="$(sed -n 's/^public_port=//p' "$file" | tail -1)"
  public_ip="$(sed -n 's/^public_ip=//p' "$file" | tail -1)"
  proto="$(sed -n 's/^proto=//p' "$file" | tail -1)"
  [[ -n "$public_port" ]] || return 1
  [[ -z "$target_port" || "$target_port" == "$PRIVATE_PORT" ]] || return 1
  [[ -z "$local_port" || "$local_port" == "$PRIVATE_PORT" ]] || return 1
  [[ -z "$proto" || "$proto" == "$PROTO" ]] || return 1
  printf 'source=%s\npublic_ip=%s\npublic_port=%s\nprivate_port=%s\nproto=%s\n' \
    "$file" "$public_ip" "$public_port" "$PRIVATE_PORT" "${proto:-$PROTO}"
}

read_gluetun_api() {
  [[ -n "$GLUETUN_API" ]] || return 1
  local json port
  json="$(curl -fsS "$GLUETUN_API/v1/openvpn/portforwarded" 2>/dev/null)" || return 1
  port="$(jq -r '.port // .forwarded_port // .forwardedPort // empty' <<<"$json")"
  [[ -n "$port" && "$port" != "0" ]] || return 1
  printf 'source=%s\npublic_ip=\npublic_port=%s\nprivate_port=%s\nproto=%s\n' \
    "$GLUETUN_API" "$port" "$PRIVATE_PORT" "$PROTO"
}

discover() {
  read_gluetun_api && return 0
  for dir in "$STATE_DIR" "$STATIC_FORWARD_DIR"; do
    [[ -d "$dir" ]] || continue
    while IFS= read -r file; do
      read_env_file "$file" && return 0
    done < <(find "$dir" -maxdepth 1 -type f -name 'pf*.env' | sort)
  done
  return 1
}

write_env() {
  local mapping="$1"
  local public_ip public_port private_port proto source
  public_ip="$(sed -n 's/^public_ip=//p' <<<"$mapping")"
  public_port="$(sed -n 's/^public_port=//p' <<<"$mapping")"
  private_port="$(sed -n 's/^private_port=//p' <<<"$mapping")"
  proto="$(sed -n 's/^proto=//p' <<<"$mapping")"
  source="$(sed -n 's/^source=//p' <<<"$mapping")"
  mkdir -p "$(dirname "$OUT_ENV")"
  cat > "$OUT_ENV" <<EOF
TNG_INCOMING_PORT=$private_port
TNG_VPN_PUBLIC_PORT=$public_port
TNG_VPN_PUBLIC_IP=$public_ip
TNG_VPN_FORWARD_PROTO=$proto
TNG_VPN_FORWARD_SOURCE=$source
EOF
  echo "$OUT_ENV"
}

cmd="${1:-print}"
[[ "$cmd" == "-h" || "$cmd" == "--help" ]] && { usage; exit 0; }
mapping="$(discover)" || {
  echo "No matching VPN forwarded-port state found for private port $PRIVATE_PORT/$PROTO" >&2
  exit 2
}

case "$cmd" in
  print)
    printf '%s\n' "$mapping"
    ;;
  write-env)
    write_env "$mapping"
    ;;
  restart-cert)
    out="$(write_env "$mapping")"
    set -a
    [[ -f "$ENV_FILE" ]] && source "$ENV_FILE"
    source "$out"
    set +a
    TNG_HOST_PORT="${TNG_HOST_PORT:-28080}" \
    SONARR_HOST_PORT="${SONARR_HOST_PORT:-28989}" \
    RADARR_HOST_PORT="${RADARR_HOST_PORT:-27878}" \
    PROWLARR_HOST_PORT="${PROWLARR_HOST_PORT:-29696}" \
    AUTOBRR_HOST_PORT="${AUTOBRR_HOST_PORT:-27474}" \
    CROSS_SEED_HOST_PORT="${CROSS_SEED_HOST_PORT:-22468}" \
    TNG_INCOMING_PORT="$TNG_INCOMING_PORT" \
      docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" up -d torrentng
    ;;
  *)
    usage >&2
    exit 64
    ;;
esac
