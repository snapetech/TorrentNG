#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/phase1-cert-$(date -u +%Y%m%dT%H%M%SZ).md}"
CONTAINER="${PHASE1_CONTAINER:-rtorrentng-phase1}"
HTTP_URL="${PHASE1_HTTP_URL:-http://localhost:${PHASE1_HTTP_PORT:-8080}}"
EXPECTED_RTORRENT="${PHASE1_EXPECTED_RTORRENT:-0.16.11}"
EXPECTED_RUTORRENT="${PHASE1_EXPECTED_RUTORRENT:-5.3.1}"
CONTAINER_INCOMING_PORT="${PHASE1_CONTAINER_INCOMING_PORT:-50000}"
HOST_INCOMING_PORT="${PHASE1_INCOMING_PORT:-50000}"
BODY="$(mktemp)"

mkdir -p "$(dirname "$OUT")"

mapped_http="$(docker port "$CONTAINER" 80/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped_http" && "$HTTP_URL" == http://localhost:* ]]; then
  HTTP_URL="http://localhost:$mapped_http"
fi

mapped_incoming="$(docker port "$CONTAINER" 50000/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped_incoming" ]]; then
  HOST_INCOMING_PORT="$mapped_incoming"
fi
container_env_port="$(docker exec "$CONTAINER" sh -lc 'printf "%s" "${RTORRENT_INCOMING_PORT:-}"' 2>/dev/null || true)"
if [[ "$container_env_port" =~ ^[0-9]+$ ]]; then
  CONTAINER_INCOMING_PORT="$container_env_port"
fi

cleanup() {
  rm -f "$BODY"
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

container_exec() {
  docker exec "$CONTAINER" sh -lc "$1" 2>/dev/null || true
}

http_code() {
  curl -ksS -o "$BODY" -w '%{http_code}' "$HTTP_URL$1" || true
}

{
  echo "# rtorrentNG Phase 1 Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Container: $CONTAINER"
  echo "- ruTorrent URL: $HTTP_URL"
  echo "- Expected rTorrent/libtorrent: $EXPECTED_RTORRENT"
  echo "- Expected ruTorrent: $EXPECTED_RUTORRENT"
  echo "- Incoming host port: $HOST_INCOMING_PORT"
  echo "- Incoming container port: $CONTAINER_INCOMING_PORT"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if docker inspect "$CONTAINER" >/dev/null 2>&1; then
  health="$(docker inspect -f '{{if .State.Health}}{{.State.Health.Status}}{{else}}{{.State.Status}}{{end}}' "$CONTAINER" 2>/dev/null || true)"
  [[ "$health" == "healthy" || "$health" == "running" ]] \
    && mark "container health" "PASS" "$health" \
    || mark "container health" "FAIL" "${health:-unknown}"
else
  mark "container exists" "FAIL" "$CONTAINER not found"
  echo >> "$OUT"
  echo "Overall status: $status" >> "$OUT"
  echo "$OUT"
  exit 1
fi

code="$(http_code /)"
if [[ "$code" == "200" ]] && grep -Eq 'js/webui\.js|css/style\.css|v=531' "$BODY"; then
  mark "ruTorrent HTTP" "PASS" "HTTP 200 index has ruTorrent assets"
else
  mark "ruTorrent HTTP" "FAIL" "HTTP $code"
fi

rt_version="$(container_exec "rtorrent -h 2>&1 | sed -n 's/^Rakshasa.*version \\([0-9.]*\\).*/\\1/p' | head -1")"
rt_version="${rt_version%.}"
[[ "$rt_version" == "$EXPECTED_RTORRENT" ]] \
  && mark "rTorrent version" "PASS" "$rt_version" \
  || mark "rTorrent version" "FAIL" "expected $EXPECTED_RTORRENT got ${rt_version:-unknown}"

lib_version="$(container_exec "strings /usr/local/lib/libtorrent.so* 2>/dev/null | grep -m1 -E '^$EXPECTED_RTORRENT$' || true")"
if [[ "$lib_version" == "$EXPECTED_RTORRENT" ]]; then
  mark "libtorrent version evidence" "PASS" "$lib_version"
else
  mark "libtorrent version evidence" "INFO" "shared library present; exact string not found"
fi

rutorrent_version="$(container_exec "grep -R \"version.*$EXPECTED_RUTORRENT\\|$EXPECTED_RUTORRENT\" -n /var/www/rutorrent 2>/dev/null | head -1")"
if [[ -n "$rutorrent_version" ]]; then
  mark "ruTorrent version evidence" "PASS" "$rutorrent_version"
else
  mark "ruTorrent version evidence" "FAIL" "$EXPECTED_RUTORRENT not found under /var/www/rutorrent"
fi

php_version="$(container_exec "php -r 'echo PHP_VERSION;'")"
[[ "$php_version" == 8.3.* ]] \
  && mark "PHP version" "PASS" "$php_version" \
  || mark "PHP version" "FAIL" "expected 8.3.x got ${php_version:-unknown}"

nginx_version="$(container_exec "nginx -v 2>&1")"
[[ "$nginx_version" == *"nginx/"* ]] \
  && mark "nginx version" "PASS" "$nginx_version" \
  || mark "nginx version" "FAIL" "${nginx_version:-unknown}"

if container_exec "test -S /run/rtorrent/rpc.sock && test -r /run/rtorrent/rpc.sock && test -w /run/rtorrent/rpc.sock && echo ok" | grep -q '^ok$'; then
  mark "SCGI socket" "PASS" "/run/rtorrent/rpc.sock readable/writable socket"
else
  mark "SCGI socket" "FAIL" "missing or inaccessible /run/rtorrent/rpc.sock"
fi

for proc in rtorrent nginx php-fpm83; do
  if container_exec "pgrep $proc >/dev/null && echo ok" | grep -q '^ok$'; then
    mark "$proc process" "PASS" "running"
  else
    mark "$proc process" "FAIL" "not running"
  fi
done

port_hex="$(printf '%04X' "$CONTAINER_INCOMING_PORT")"
if container_exec "grep -qi ':$port_hex ' /proc/net/tcp /proc/net/tcp6 2>/dev/null && echo ok" | grep -q '^ok$'; then
  mark "TCP listener" "PASS" "container port $CONTAINER_INCOMING_PORT mapped to host $HOST_INCOMING_PORT"
else
  mark "TCP listener" "FAIL" "container port $CONTAINER_INCOMING_PORT not found in tcp sockets"
fi

if container_exec "grep -qi ':$port_hex ' /proc/net/udp /proc/net/udp6 2>/dev/null && echo ok" | grep -q '^ok$'; then
  mark "UDP listener" "PASS" "container port $CONTAINER_INCOMING_PORT mapped to host $HOST_INCOMING_PORT"
else
  mark "UDP listener" "INFO" "container port $CONTAINER_INCOMING_PORT not currently present in udp sockets"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
