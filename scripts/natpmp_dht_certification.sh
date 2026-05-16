#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/natpmp-dht-$(date -u +%Y%m%dT%H%M%SZ).md}"
GATEWAY="${RTNG_NATPMP_GATEWAY:-$(ip route | awk '/^default / {print $3; exit}')}"
PUBLIC_PORT="${RTNG_NATPMP_PUBLIC_PORT:-${RTNG_INCOMING_PORT:-51000}}"
PRIVATE_PORT="${RTNG_NATPMP_PRIVATE_PORT:-${RTNG_INCOMING_PORT:-51000}}"
LIFETIME="${RTNG_NATPMP_LIFETIME:-3600}"
RTNG_HOST_URL="${RTNG_HOST_URL:-http://localhost:${RTNG_HOST_PORT:-28080}}"
TCP_LOG="$(mktemp)"
UDP_LOG="$(mktemp)"
DHT_REPORT="$ROOT/certification/reports/dht-cert-natpmp-$(date -u +%Y%m%dT%H%M%SZ).md"

mkdir -p "$(dirname "$OUT")"

cleanup() {
  rm -f "$TCP_LOG" "$UDP_LOG"
}
trap cleanup EXIT

status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

{
  echo "# rtorrentNG NAT-PMP DHT Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Gateway: ${GATEWAY:-unknown}"
  echo "- Public port: $PUBLIC_PORT"
  echo "- Private port: $PRIVATE_PORT"
  echo "- Lifetime seconds: $LIFETIME"
  echo "- rtorrentNG URL: $RTNG_HOST_URL"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if [[ -z "$GATEWAY" ]]; then
  mark "NAT-PMP gateway" "FAIL" "no default gateway found"
else
  mark "NAT-PMP gateway" "PASS" "$GATEWAY"
fi

if natpmpc -g "$GATEWAY" -a "$PUBLIC_PORT" "$PRIVATE_PORT" tcp "$LIFETIME" >"$TCP_LOG" 2>&1; then
  public_ip="$(sed -n 's/^Public IP address : //p' "$TCP_LOG" | tail -1)"
  mapped_tcp="$(sed -n 's/^Mapped public port //p' "$TCP_LOG" | tail -1)"
  mark "TCP NAT-PMP mapping" "PASS" "${mapped_tcp:-created}"
else
  public_ip=""
  mark "TCP NAT-PMP mapping" "FAIL" "$(tr '\n' ' ' <"$TCP_LOG")"
fi

if natpmpc -g "$GATEWAY" -a "$PUBLIC_PORT" "$PRIVATE_PORT" udp "$LIFETIME" >"$UDP_LOG" 2>&1; then
  [[ -n "$public_ip" ]] || public_ip="$(sed -n 's/^Public IP address : //p' "$UDP_LOG" | tail -1)"
  mapped_udp="$(sed -n 's/^Mapped public port //p' "$UDP_LOG" | tail -1)"
  mark "UDP NAT-PMP mapping" "PASS" "${mapped_udp:-created}"
else
  mark "UDP NAT-PMP mapping" "FAIL" "$(tr '\n' ' ' <"$UDP_LOG")"
fi

if timeout 5 nc -vz 127.0.0.1 "$PRIVATE_PORT" >/tmp/rtng-natpmp-local-tcp.log 2>&1; then
  mark "local TCP listener" "PASS" "127.0.0.1:$PRIVATE_PORT"
else
  mark "local TCP listener" "FAIL" "$(tr '\n' ' ' </tmp/rtng-natpmp-local-tcp.log)"
fi

if [[ -n "$public_ip" ]]; then
  if timeout 5 nc -vz "$public_ip" "$PUBLIC_PORT" >/tmp/rtng-natpmp-public-tcp.log 2>&1; then
    mark "public TCP hairpin probe" "PASS" "$public_ip:$PUBLIC_PORT"
  else
    mark "public TCP hairpin probe" "INFO" "$(tr '\n' ' ' </tmp/rtng-natpmp-public-tcp.log)"
  fi
else
  mark "public TCP hairpin probe" "INFO" "public IP unavailable"
fi

if [[ -n "$public_ip" ]] && RTNG_HOST_URL="$RTNG_HOST_URL" RTNG_INCOMING_PORT="$PRIVATE_PORT" RTNG_VPN_PUBLIC_PORT="$PUBLIC_PORT" RTNG_VPN_PUBLIC_IP="$public_ip" "$ROOT/scripts/dht_certification.sh" "$DHT_REPORT"; then
  mark "DHT certification with mapped endpoint" "PASS" "$(basename "$DHT_REPORT")"
else
  mark "DHT certification with mapped endpoint" "FAIL" "$(basename "$DHT_REPORT")"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
