#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/dht-cert-$(date -u +%Y%m%dT%H%M%SZ).md}"

ENV_RTNG_HOST_URL="${RTNG_HOST_URL:-}"
if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

RTNG_HOST_URL="${ENV_RTNG_HOST_URL:-${RTNG_HOST_URL:-http://localhost:${RTNG_HOST_PORT:-18080}}}"
RTNG_API_TOKEN="${RTNG_API_TOKEN:-cert-token}"
RTNG_CONTAINER="${RTNG_CONTAINER:-certification-rtorrentng-1}"
PUBLIC_PORT="${RTNG_VPN_PUBLIC_PORT:-${RTNG_INCOMING_PORT:-50000}}"
PRIVATE_PORT="${RTNG_PRIVATE_INCOMING_PORT:-${RTNG_INCOMING_PORT:-$PUBLIC_PORT}}"
PUBLIC_IP="${RTNG_VPN_PUBLIC_IP:-}"
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
  echo "# rtorrentNG DHT / Public-Port Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- rtorrentNG URL: $RTNG_HOST_URL"
  echo "- Expected public port: $PUBLIC_PORT"
  echo "- Expected private listen port: $PRIVATE_PORT"
  [[ -n "$PUBLIC_IP" ]] && echo "- Expected public IP: $PUBLIC_IP"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

engine="$(curl -fsS -H "Authorization: Bearer $RTNG_API_TOKEN" "$RTNG_HOST_URL/api/v1/engine")"
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
if docker exec "$RTNG_CONTAINER" sh -lc "grep -qi ':$port_hex ' /proc/net/udp /proc/net/udp6 2>/dev/null"; then
  mark "container UDP listener" "PASS" "UDP $PRIVATE_PORT bound in rtorrentng container"
else
  mark "container UDP listener" "FAIL" "UDP $PRIVATE_PORT not bound in rtorrentng container"
fi

if [[ -n "$PUBLIC_IP" ]]; then
  observed_ip="$(timeout 12 docker exec "$RTNG_CONTAINER" sh -lc 'wget -T 5 -qO- https://ifconfig.me/ip 2>/dev/null || wget -T 5 -qO- https://api.ipify.org 2>/dev/null || true' | tr -d '\r\n' || true)"
  [[ "$observed_ip" == "$PUBLIC_IP" ]] \
    && mark "VPN egress IP" "PASS" "$observed_ip" \
    || mark "VPN egress IP" "FAIL" "expected $PUBLIC_IP got ${observed_ip:-unknown}"
else
  mark "VPN egress IP" "INFO" "RTNG_VPN_PUBLIC_IP not supplied"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
