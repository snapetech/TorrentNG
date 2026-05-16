#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SLSKR_ROOT="${SLSKR_ROOT:-/home/keith/Documents/code/slskR}"
POOL_FILE="${SLSKR_PROTON_CREDENTIAL_POOL_FILE:-$SLSKR_ROOT/.secrets/proton-credential-pool.env}"
LABEL="${RTNG_PROTON_LABEL:-p1}"
OUT="${1:-$ROOT/certification/reports/proton-natpmp-$(date -u +%Y%m%dT%H%M%SZ).md}"
PRIVATE_PORT="${RTNG_PROTON_PRIVATE_PORT:-${RTNG_INCOMING_PORT:-51000}}"
REQUESTED_PUBLIC_PORT="${RTNG_PROTON_PUBLIC_PORT:-0}"
LIFETIME="${RTNG_PROTON_NATPMP_LIFETIME:-120}"
GATEWAY="${RTNG_PROTON_NATPMP_GATEWAY:-10.2.0.1}"
NAMESPACE="rtng-${LABEL}-$(date -u +%H%M%S)"
RTNG_CONTAINER="${RTNG_CONTAINER:-certification-rtorrentng-1}"
TMP_OUTPUT="$(mktemp)"

mkdir -p "$(dirname "$OUT")"

cleanup() {
  rm -f "$TMP_OUTPUT"
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
  echo "# rtorrentNG Proton NAT-PMP Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Proton label: $LABEL"
  echo "- Namespace: $NAMESPACE"
  echo "- NAT-PMP gateway: $GATEWAY"
  echo "- Requested public port: $REQUESTED_PUBLIC_PORT"
  echo "- Private port: $PRIVATE_PORT"
  echo "- Lifetime seconds: $LIFETIME"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if [[ ! -f "$POOL_FILE" ]]; then
  mark "credential pool" "FAIL" "missing $POOL_FILE"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi

set -a
# shellcheck disable=SC1090
source "$POOL_FILE"
set +a

config_var="SLSKR_PROTON_CONFIG_${LABEL}"
config_path="${!config_var:-}"
if [[ -z "$config_path" ]]; then
  mark "Proton config" "FAIL" "$config_var is not set"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi
if [[ "$config_path" != /* ]]; then
  config_path="$SLSKR_ROOT/$config_path"
fi
if [[ ! -f "$config_path" ]]; then
  mark "Proton config" "FAIL" "configured file missing for $LABEL"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi
mark "Proton config" "PASS" "$LABEL"

set +e
"$SLSKR_ROOT/scripts/run-in-proton-wg-netns.sh" "$NAMESPACE" "$config_path" \
  bash -lc '
    set -euo pipefail
    gateway="$1"
    requested="$2"
    private="$3"
    lifetime="$4"
    egress="$(curl -fsS --max-time 10 https://api.ipify.org 2>/dev/null || true)"
    echo "egress=$egress"
    natpmpc -g "$gateway" -a "$requested" "$private" tcp "$lifetime"
    natpmpc -g "$gateway" -a "$requested" "$private" udp "$lifetime"
  ' bash "$GATEWAY" "$REQUESTED_PUBLIC_PORT" "$PRIVATE_PORT" "$LIFETIME" >"$TMP_OUTPUT" 2>&1
ns_status=$?
set -e

if [[ "$ns_status" -eq 0 ]]; then
  mark "Proton namespace command" "PASS" "completed"
else
  mark "Proton namespace command" "FAIL" "$(tail -20 "$TMP_OUTPUT")"
fi

proton_ip="$(sed -n 's/^egress=//p' "$TMP_OUTPUT" | tail -1)"
[[ -n "$proton_ip" ]] || proton_ip="$(sed -n 's/^Public IP address : //p' "$TMP_OUTPUT" | tail -1)"
[[ -n "$proton_ip" ]] && mark "Proton egress IP" "PASS" "$proton_ip" || mark "Proton egress IP" "FAIL" "missing"

tcp_mapping="$(awk '/Mapped public port/ && /protocol TCP/ {for (i=1; i<=NF; i++) if ($i=="port") {print $(i+1); exit}}' "$TMP_OUTPUT")"
udp_mapping="$(awk '/Mapped public port/ && /protocol UDP/ {for (i=1; i<=NF; i++) if ($i=="port") {print $(i+1); exit}}' "$TMP_OUTPUT")"

[[ -n "$tcp_mapping" ]] \
  && mark "TCP Proton NAT-PMP mapping" "PASS" "public=$tcp_mapping private=$PRIVATE_PORT" \
  || mark "TCP Proton NAT-PMP mapping" "FAIL" "missing"
[[ -n "$udp_mapping" ]] \
  && mark "UDP Proton NAT-PMP mapping" "PASS" "public=$udp_mapping private=$PRIVATE_PORT" \
  || mark "UDP Proton NAT-PMP mapping" "FAIL" "missing"

container_ip="$(docker exec "$RTNG_CONTAINER" sh -lc 'wget -qO- https://api.ipify.org 2>/dev/null || true' 2>/dev/null | tr -d '\r\n')"
if [[ -n "$container_ip" && -n "$proton_ip" && "$container_ip" == "$proton_ip" ]]; then
  mark "rtorrentNG VPN alignment" "PASS" "container egress matches Proton egress $container_ip"
else
  mark "rtorrentNG VPN alignment" "INFO" "container egress=${container_ip:-unknown} proton egress=${proton_ip:-unknown}; current rtorrentNG container is not in this Proton namespace"
fi

{
  echo
  echo "## Raw NAT-PMP Output"
  echo
  echo '```text'
  sed -E 's/(PrivateKey|PresharedKey|Password|Token|Secret)[^[:space:]]*/\1=<redacted>/Ig' "$TMP_OUTPUT"
  echo '```'
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
