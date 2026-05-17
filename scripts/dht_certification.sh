#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/dht-cert-$(date -u +%Y%m%dT%H%M%SZ).md}"

ENV_TNG_HOST_URL="${TNG_HOST_URL:-}"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_HOST_URL="${ENV_TNG_HOST_URL:-${TNG_HOST_URL:-http://localhost:${TNG_HOST_PORT:-18080}}}"
TNG_API_TOKEN="${TNG_API_TOKEN:-cert-token}"
TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
PUBLIC_PORT="${TNG_VPN_PUBLIC_PORT:-${TNG_INCOMING_PORT:-50000}}"
PRIVATE_PORT="${TNG_PRIVATE_INCOMING_PORT:-${TNG_INCOMING_PORT:-$PUBLIC_PORT}}"
PUBLIC_IP="${TNG_VPN_PUBLIC_IP:-}"
COMPOSE_FILE="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"

mkdir -p "$(dirname "$OUT")"
status="PASS"
mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  [[ "$result" == "FAIL" ]] && status="FAIL"
  return 0
}

{
  echo "# TorrentNG DHT / Public-Port Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- TorrentNG URL: $TNG_HOST_URL"
  echo "- Expected public port: $PUBLIC_PORT"
  echo "- Expected private listen port: $PRIVATE_PORT"
  [[ -n "$PUBLIC_IP" ]] && echo "- Expected public IP: $PUBLIC_IP"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

engine="$(curl -fsS -H "Authorization: Bearer $TNG_API_TOKEN" "$TNG_HOST_URL/api/v1/engine")"
dht_port="$(jq -r '.dht.port.value // empty' <<<"$engine")"
override_port="$(jq -r '.dht.override_port.value // empty' <<<"$engine")"
listen_range="$(jq -r '.dht.listen_range.value // empty' <<<"$engine")"
pex="$(jq -r '.dht.pex.value // false' <<<"$engine")"
udp_trackers="$(jq -r '.dht.udp_trackers.value // false' <<<"$engine")"

[[ "$listen_range" == "$PRIVATE_PORT-$PRIVATE_PORT" ]] \
  && mark "rTorrent listen range" "PASS" "$listen_range" \
  || mark "rTorrent listen range" "FAIL" "expected $PRIVATE_PORT-$PRIVATE_PORT got ${listen_range:-unknown}"

[[ "$dht_port" == "$PRIVATE_PORT" || "$override_port" == "$PRIVATE_PORT" ]] \
  && mark "rTorrent DHT port" "PASS" "dht.port=${dht_port:-unknown} override=${override_port:-none}" \
  || mark "rTorrent DHT port" "FAIL" "expected $PRIVATE_PORT got dht.port=${dht_port:-unknown} override=${override_port:-none}"

[[ "$pex" == "true" ]] \
  && mark "PEX enabled" "PASS" "protocol.pex=true" \
  || mark "PEX enabled" "FAIL" "protocol.pex=$pex"

[[ "$udp_trackers" == "true" ]] \
  && mark "UDP trackers enabled" "PASS" "trackers.use_udp=true" \
  || mark "UDP trackers enabled" "FAIL" "trackers.use_udp=$udp_trackers"

port_hex="$(printf '%04X' "$PRIVATE_PORT")"
if docker exec "$TNG_CONTAINER" sh -lc "grep -qi ':$port_hex ' /proc/net/udp /proc/net/udp6 2>/dev/null"; then
  mark "container UDP listener" "PASS" "UDP $PRIVATE_PORT bound in torrentng container"
else
  mark "container UDP listener" "FAIL" "UDP $PRIVATE_PORT not bound in torrentng container"
fi

if [[ -n "$PUBLIC_IP" ]]; then
  observed_ip="$(timeout 12 docker exec "$TNG_CONTAINER" sh -lc 'wget -T 5 -qO- https://ifconfig.me/ip 2>/dev/null || wget -T 5 -qO- https://api.ipify.org 2>/dev/null || true' | tr -d '\r\n' || true)"
  [[ "$observed_ip" == "$PUBLIC_IP" ]] \
    && mark "VPN egress IP" "PASS" "$observed_ip" \
    || mark "VPN egress IP" "FAIL" "expected $PUBLIC_IP got ${observed_ip:-unknown}"
else
  mark "VPN egress IP" "INFO" "TNG_VPN_PUBLIC_IP not supplied"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
