#!/usr/bin/env bash
# Diagnostic script: check rTorrent socket, SCGI connectivity, and sidecar health.

set -euo pipefail

SOCKET="${1:-/run/rtorrent/rpc.sock}"
SIDECAR="${2:-http://localhost:8080}"
PASS=0
FAIL=0

ok()   { echo "  [OK]  $*"; ((PASS++)); }
fail() { echo "  [FAIL] $*"; ((FAIL++)); }
info() { echo "  [--]  $*"; }

echo "=== rtorrentNG healthcheck ==="
echo

echo "--- rTorrent socket ---"
if [ -S "$SOCKET" ]; then
  ok "Socket exists: $SOCKET"
  PERMS=$(stat -c "%a" "$SOCKET")
  if [[ "$PERMS" == "660" || "$PERMS" == "600" || "$PERMS" == "770" || "$PERMS" == "700" ]]; then
    ok "Socket permissions ($PERMS) — not world-readable"
  else
    fail "Socket permissions ($PERMS) — may be too open or too restrictive"
  fi
else
  fail "Socket not found: $SOCKET"
fi

echo
echo "--- SCGI / XMLRPC ---"
if command -v curl &>/dev/null; then
  XMLRPC_BODY='<?xml version="1.0"?><methodCall><methodName>system.listMethods</methodName><params/></methodCall>'
  HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" --unix-socket "$SOCKET" \
    -X POST -H "Content-Type: text/xml" \
    --data "$XMLRPC_BODY" "http://localhost/RPC2" 2>/dev/null || echo "000")
  if [ "$HTTP_STATUS" = "200" ]; then
    ok "XMLRPC call succeeded (HTTP 200)"
  else
    fail "XMLRPC call failed (HTTP $HTTP_STATUS)"
  fi
else
  info "curl not available — skipping XMLRPC test"
fi

echo
echo "--- Sidecar API ---"
if command -v curl &>/dev/null; then
  HEALTH=$(curl -s "$SIDECAR/health" 2>/dev/null || echo '{}')
  STATUS=$(echo "$HEALTH" | grep -o '"status":"[^"]*"' | cut -d'"' -f4)
  RT=$(echo "$HEALTH" | grep -o '"rtorrent":"[^"]*"' | cut -d'"' -f4)
  CACHED=$(echo "$HEALTH" | grep -o '"cached_torrents":[0-9]*' | cut -d: -f2)
  if [ "$STATUS" = "ok" ]; then
    ok "Sidecar health: $STATUS"
  else
    fail "Sidecar health: ${STATUS:-unreachable}"
  fi
  if [ "$RT" = "connected" ]; then
    ok "rTorrent connection: $RT"
  else
    fail "rTorrent connection: ${RT:-unknown}"
  fi
  info "Cached torrents: ${CACHED:-unknown}"
else
  info "curl not available — skipping sidecar test"
fi

echo
echo "=== Result: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
