#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
SLSKR_ROOT="${SLSKR_ROOT:-/home/keith/Documents/code/slskR}"
POOL_FILE="${SLSKR_PROTON_CREDENTIAL_POOL_FILE:-$SLSKR_ROOT/.secrets/proton-credential-pool.env}"
LABEL="${TNG_PROTON_LABEL:-p1}"
OUT="${1:-$ROOT/certification/reports/proton-tng-dht-$(date -u +%Y%m%dT%H%M%SZ).md}"
IMAGE="${TNG_PROTON_IMAGE:-torrentng:certification}"
CONTAINER="tng-proton-cert-${LABEL}-$(date -u +%H%M%S)"
NS="tngvpn-${LABEL}-$(date -u +%H%M%S)"
PRIVATE_PORT="${TNG_PROTON_PRIVATE_PORT:-51000}"
NATPMP_PUBLIC_PORT="${TNG_PROTON_PUBLIC_PORT:-0}"
NATPMP_GATEWAY="${TNG_PROTON_NATPMP_GATEWAY:-10.2.0.1}"
NATPMP_LIFETIME="${TNG_PROTON_NATPMP_LIFETIME:-600}"
API_TOKEN="${TNG_API_TOKEN:-cert-token}"
SECRET_KEY="${TNG_SECRET_KEY:-proton-cert-secret-000000000000000000000000}"
SUBNET_BASE="${TNG_PROTON_SUBNET_BASE:-10.244.$((100 + ($(date +%S) % 100))).0}"
HOST_NS_IP="${TNG_PROTON_HOST_NS_IP:-${SUBNET_BASE%.*}.1}"
CONTAINER_IP="${TNG_PROTON_CONTAINER_IP:-${SUBNET_BASE%.*}.2}"
UPLINK_BASE="${TNG_PROTON_UPLINK_BASE:-10.245.$((100 + ($(date +%S) % 100))).0}"
UPLINK_HOST_IP="${TNG_PROTON_UPLINK_HOST_IP:-${UPLINK_BASE%.*}.1}"
UPLINK_NS_IP="${TNG_PROTON_UPLINK_NS_IP:-${UPLINK_BASE%.*}.2}"
VETH_HOST="v${NS:0:10}h"
VETH_CONT="v${NS:0:10}c"
VETH_UP_HOST="u${NS:0:10}h"
VETH_UP_NS="u${NS:0:10}n"
TMP_CONFIG="$(mktemp)"
TMP_OUTPUT="$(mktemp)"
DATA_DIR="$(mktemp -d)"
SESSION_DIR="$(mktemp -d)"
TNG_DATA_DIR="$(mktemp -d)"
key_file=""

mkdir -p "$(dirname "$OUT")"

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

cleanup() {
  docker rm -f "$CONTAINER" >/dev/null 2>&1 || true
  sudo ip netns pids "$NS" 2>/dev/null | xargs -r sudo kill 2>/dev/null || true
  sudo ip netns del "$NS" 2>/dev/null || true
  sudo rm -rf "/etc/netns/$NS" 2>/dev/null || true
  sudo ip link del "$VETH_CONT" 2>/dev/null || true
  sudo ip link del "$VETH_UP_HOST" 2>/dev/null || true
  sudo ip route del "$ENDPOINT_IP/32" 2>/dev/null || true
  sudo iptables -t nat -D POSTROUTING -s "${UPLINK_BASE%.*}.0/24" -j MASQUERADE 2>/dev/null || true
  sudo iptables -D FORWARD -i "$VETH_UP_HOST" -j ACCEPT 2>/dev/null || true
  sudo iptables -D FORWARD -o "$VETH_UP_HOST" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || true
  rm -f "$TMP_CONFIG" "$TMP_OUTPUT" "$key_file"
  rm -rf "$DATA_DIR" "$SESSION_DIR" "$TNG_DATA_DIR"
}
trap cleanup EXIT

extract_first_value() {
  local key="$1"
  awk -v key="$key" '
    $0 ~ "^[[:space:]]*" key "[[:space:]]*=" {
      value=$0
      sub("^[[:space:]]*" key "[[:space:]]*=[[:space:]]*", "", value)
      sub(/^[[:space:]]*/, "", value)
      sub(/[[:space:]]*$/, "", value)
      split(value, parts, ",")
      sub(/^[[:space:]]*/, "", parts[1])
      sub(/[[:space:]]*$/, "", parts[1])
      print parts[1]
      exit
    }
  ' "$CONFIG_PATH"
}

{
  echo "# TorrentNG Proton-Routed DHT Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Proton label: $LABEL"
  echo "- Namespace: $NS"
  echo "- Container: $CONTAINER"
  echo "- Namespace URL: http://$CONTAINER_IP:8080"
  echo "- Container VPN IP: $CONTAINER_IP"
  echo "- Private DHT/listen port: $PRIVATE_PORT"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if [[ ! -f "$POOL_FILE" ]]; then
  mark "credential pool" "FAIL" "missing $POOL_FILE"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

set -a
# shellcheck disable=SC1090
source "$POOL_FILE"
set +a
config_var="SLSKR_PROTON_CONFIG_${LABEL}"
CONFIG_PATH="${!config_var:-}"
[[ "$CONFIG_PATH" != /* ]] && CONFIG_PATH="$SLSKR_ROOT/$CONFIG_PATH"
if [[ ! -f "$CONFIG_PATH" ]]; then
  mark "Proton config" "FAIL" "missing config for $LABEL"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi
mark "Proton config" "PASS" "$LABEL"

PRIVATE_KEY="$(extract_first_value PrivateKey)"
ADDRESS="$(extract_first_value Address)"
PEER_PUBLIC_KEY="$(extract_first_value PublicKey)"
ENDPOINT="$(extract_first_value Endpoint)"
ENDPOINT_IP="$(sed -E 's/^[[]?([^]]+)[]]?:[0-9]+$/\1/' <<<"$ENDPOINT")"

sudo ip netns add "$NS"
sudo mkdir -p "/etc/netns/$NS"
printf 'nameserver %s\n' "$NATPMP_GATEWAY" | sudo tee "/etc/netns/$NS/resolv.conf" >/dev/null
sudo ip link add "$VETH_UP_HOST" type veth peer name "$VETH_UP_NS"
sudo ip link set "$VETH_UP_NS" netns "$NS"
sudo ip addr add "$UPLINK_HOST_IP/24" dev "$VETH_UP_HOST"
sudo ip link set "$VETH_UP_HOST" up
sudo ip netns exec "$NS" ip addr add "$UPLINK_NS_IP/24" dev "$VETH_UP_NS"
sudo ip netns exec "$NS" ip link set "$VETH_UP_NS" up
sudo ip netns exec "$NS" ip link set lo up
sudo ip netns exec "$NS" ip route replace default via "$UPLINK_HOST_IP" dev "$VETH_UP_NS"
sudo sysctl -q net.ipv4.ip_forward=1
sudo iptables -t nat -C POSTROUTING -s "${UPLINK_BASE%.*}.0/24" -j MASQUERADE 2>/dev/null || \
  sudo iptables -t nat -A POSTROUTING -s "${UPLINK_BASE%.*}.0/24" -j MASQUERADE
sudo iptables -C FORWARD -i "$VETH_UP_HOST" -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -i "$VETH_UP_HOST" -j ACCEPT
sudo iptables -C FORWARD -o "$VETH_UP_HOST" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT 2>/dev/null || \
  sudo iptables -A FORWARD -o "$VETH_UP_HOST" -m conntrack --ctstate RELATED,ESTABLISHED -j ACCEPT

default_line="$(ip route show default | awk 'NR == 1 {print}')"
default_via="$(awk '{for (i=1;i<=NF;i++) if ($i=="via") {print $(i+1); exit}}' <<<"$default_line")"
default_dev="$(awk '{for (i=1;i<=NF;i++) if ($i=="dev") {print $(i+1); exit}}' <<<"$default_line")"
if [[ "$ENDPOINT_IP" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ && -n "$default_dev" ]]; then
  if [[ -n "$default_via" ]]; then
    sudo ip route replace "$ENDPOINT_IP/32" via "$default_via" dev "$default_dev"
  else
    sudo ip route replace "$ENDPOINT_IP/32" dev "$default_dev"
  fi
  sudo ip netns exec "$NS" ip route replace "$ENDPOINT_IP/32" via "$UPLINK_HOST_IP" dev "$VETH_UP_NS"
fi

key_file="$(mktemp)"
chmod 600 "$key_file"
printf '%s\n' "$PRIVATE_KEY" > "$key_file"
sudo ip netns exec "$NS" ip link add wg0 type wireguard
sudo ip netns exec "$NS" ip addr add "$ADDRESS" dev wg0
sudo ip netns exec "$NS" wg set wg0 private-key "$key_file" peer "$PEER_PUBLIC_KEY" endpoint "$ENDPOINT" allowed-ips 0.0.0.0/0 persistent-keepalive 25
sudo ip netns exec "$NS" ip link set mtu 1420 up dev wg0
sudo ip netns exec "$NS" ip route replace default dev wg0
sudo ip netns exec "$NS" bash -lc 'timeout 3 bash -c "</dev/udp/1.1.1.1/53" 2>/dev/null || true'
sleep 2
mark "Proton namespace" "PASS" "wg0 up"

docker run -d --rm --name "$CONTAINER" --network none \
  -v "$ROOT/deploy/certification/sidecar.config.toml:/config/config.toml:ro" \
  -v "$DATA_DIR:/data" \
  -v "$SESSION_DIR:/session" \
  -v "$TNG_DATA_DIR:/var/lib/torrentng" \
  -e TORRENTNG_CONFIG=/config/config.toml \
  -e RTORRENT_INCOMING_PORT="$PRIVATE_PORT" \
  -e RTORRENT_SCGI_SOCKET=/run/rtorrent/rpc.sock \
  -e TNG_STATIC_DIR=/usr/share/torrentng/webui \
  -e TNG_SYNC_INTERVAL_SECS=2 \
  -e TNG_DATA_DIR=/var/lib/torrentng \
  -e TNG_SECRET_KEY="$SECRET_KEY" \
  -e TNG_API_TOKENS="$API_TOKEN" \
  "$IMAGE" >/dev/null
docker exec "$CONTAINER" sh -lc "printf 'nameserver %s\n' '$NATPMP_GATEWAY' > /etc/resolv.conf" 2>/dev/null || true
mark "TorrentNG container" "PASS" "$IMAGE"

container_pid="$(docker inspect -f '{{.State.Pid}}' "$CONTAINER")"
sudo ip link add "$VETH_HOST" type veth peer name "$VETH_CONT"
sudo ip link set "$VETH_HOST" netns "$NS"
sudo ip link set "$VETH_CONT" netns "$container_pid"
sudo ip netns exec "$NS" ip addr add "$HOST_NS_IP/24" dev "$VETH_HOST"
sudo ip netns exec "$NS" ip link set "$VETH_HOST" up
sudo ip netns exec "$NS" sysctl -q net.ipv4.ip_forward=1
sudo ip netns exec "$NS" iptables -t nat -A POSTROUTING -s "$CONTAINER_IP/32" -o wg0 -j MASQUERADE
sudo nsenter -t "$container_pid" -n ip addr add "$CONTAINER_IP/24" dev "$VETH_CONT"
sudo nsenter -t "$container_pid" -n ip link set "$VETH_CONT" up
sudo nsenter -t "$container_pid" -n ip link set lo up
sudo nsenter -t "$container_pid" -n ip route replace default via "$HOST_NS_IP" dev "$VETH_CONT"
mark "container VPN route" "PASS" "$CONTAINER_IP via $HOST_NS_IP -> $NS/wg0"

for _ in $(seq 1 60); do
  code="$(sudo ip netns exec "$NS" curl -ksS -o /dev/null -w '%{http_code}' "http://$CONTAINER_IP:8080/health" || true)"
  [[ "$code" == "200" || "$code" == "503" ]] && break
  sleep 1
done
[[ "$code" == "200" || "$code" == "503" ]] \
  && mark "TorrentNG health" "PASS" "HTTP $code" \
  || mark "TorrentNG health" "FAIL" "HTTP $code"

container_egress="$(timeout 12 docker exec "$CONTAINER" sh -lc 'wget -T 5 -qO- https://api.ipify.org 2>/dev/null || wget -T 5 -qO- http://ifconfig.me/ip 2>/dev/null || true' | tr -d '\r\n' || true)"
[[ -n "$container_egress" ]] \
  && mark "container egress" "PASS" "$container_egress" \
  || mark "container egress" "INFO" "external IP lookup unavailable from container"

set +e
timeout 20 sudo ip netns exec "$NS" natpmpc -g "$NATPMP_GATEWAY" -a "$NATPMP_PUBLIC_PORT" "$PRIVATE_PORT" tcp "$NATPMP_LIFETIME" > "$TMP_OUTPUT" 2>&1
tcp_natpmp_status=$?
timeout 20 sudo ip netns exec "$NS" natpmpc -g "$NATPMP_GATEWAY" -a "$NATPMP_PUBLIC_PORT" "$PRIVATE_PORT" udp "$NATPMP_LIFETIME" >> "$TMP_OUTPUT" 2>&1
udp_natpmp_status=$?
set -e
public_ip="$(sed -n 's/^Public IP address : //p' "$TMP_OUTPUT" | tail -1)"
public_port="$(awk '/Mapped public port/ {for (i=1; i<=NF; i++) if ($i=="port") {print $(i+1); exit}}' "$TMP_OUTPUT")"
[[ "$tcp_natpmp_status" -eq 0 && "$udp_natpmp_status" -eq 0 && -n "$public_ip" && -n "$public_port" ]] \
  && mark "Proton NAT-PMP mapping" "PASS" "$public_ip:$public_port -> $PRIVATE_PORT tcp/udp" \
  || mark "Proton NAT-PMP mapping" "FAIL" "tcp_exit=$tcp_natpmp_status udp_exit=$udp_natpmp_status $(tr '\n' ' ' < "$TMP_OUTPUT")"

DHT_REPORT="$ROOT/certification/reports/dht-cert-proton-tng-$(date -u +%Y%m%dT%H%M%SZ).md"
if [[ -n "$public_ip" && -n "$public_port" ]] && sudo -E ip netns exec "$NS" env PATH="$PATH" TNG_HOST_URL="http://$CONTAINER_IP:8080" TNG_API_TOKEN="$API_TOKEN" TNG_CONTAINER="$CONTAINER" TNG_INCOMING_PORT="$PRIVATE_PORT" TNG_VPN_PUBLIC_PORT="$public_port" TNG_VPN_PUBLIC_IP="$public_ip" "$ROOT/scripts/dht_certification.sh" "$DHT_REPORT"; then
  mark "DHT certification over Proton-routed TorrentNG" "PASS" "$(basename "$DHT_REPORT")"
else
  mark "DHT certification over Proton-routed TorrentNG" "FAIL" "$(basename "$DHT_REPORT")"
fi

{
  echo
  echo "## Raw NAT-PMP Output"
  echo
  echo '```text'
  cat "$TMP_OUTPUT"
  echo '```'
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
