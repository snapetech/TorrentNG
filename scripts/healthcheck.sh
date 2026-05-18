#!/usr/bin/env bash
# Diagnostic script: check selected TorrentNG sidecar backend and HTTP health.

set -euo pipefail

SOCKET="${1:-/run/rtorrent/rpc.sock}"
SIDECAR="${2:-http://localhost:8080}"
RUTORRENT="${3:-http://localhost:8080/rutorrent/}"
BACKEND="${TNG_BACKEND:-rtorrent}"
BACKEND_LOWER="$(printf '%s' "$BACKEND" | tr '[:upper:]' '[:lower:]')"
PASS=0
FAIL=0

ok()   { echo "  [OK]  $*"; ((PASS++)); }
fail() { echo "  [FAIL] $*"; ((FAIL++)); }
info() { echo "  [--]  $*"; }

echo "=== TorrentNG healthcheck ==="
echo

if [ "$BACKEND_LOWER" = "rtorrent" ]; then
  echo "--- rTorrent socket ---"
  if [ -S "$SOCKET" ]; then
    ok "Socket exists: $SOCKET"
    PERMS=$(stat -c "%a" "$SOCKET")
    if [[ "$PERMS" == "660" || "$PERMS" == "600" || "$PERMS" == "770" || "$PERMS" == "700" ]]; then
      ok "Socket permissions ($PERMS) - not world-readable"
    else
      fail "Socket permissions ($PERMS) - may be too open or too restrictive"
    fi
  else
    fail "Socket not found: $SOCKET"
  fi

  echo
  echo "--- SCGI / XMLRPC ---"
  if command -v curl &>/dev/null; then
    if [ -S "$SOCKET" ]; then
      info "SCGI socket is present. Raw XMLRPC probe requires a SCGI-capable client."
    else
      fail "Cannot probe XMLRPC because socket is missing"
    fi
  else
    info "curl not available - skipping XMLRPC note"
  fi

  echo
  echo "--- ruTorrent / nginx ---"
  if command -v curl &>/dev/null; then
    HTTP_STATUS=$(curl -s -o /dev/null -w "%{http_code}" "$RUTORRENT" 2>/dev/null || echo "000")
    if [ "$HTTP_STATUS" = "200" ]; then
      ok "ruTorrent reachable: $RUTORRENT"
    else
      fail "ruTorrent not reachable at $RUTORRENT (HTTP $HTTP_STATUS)"
    fi
  else
    info "curl not available - skipping ruTorrent test"
  fi
else
  echo "--- External backend ---"
  info "Selected backend: $BACKEND"
  info "Skipping rTorrent socket and ruTorrent checks for external backend mode"
fi

echo
echo "--- Sidecar API ---"
if command -v curl &>/dev/null; then
  HEALTH=$(curl -s "$SIDECAR/health" 2>/dev/null || echo '{}')
  if command -v jq &>/dev/null; then
    STATUS=$(printf '%s' "$HEALTH" | jq -r '.status // empty' 2>/dev/null || true)
    BACKEND_TYPE=$(printf '%s' "$HEALTH" | jq -r '.backend.type // empty' 2>/dev/null || true)
    BACKEND_STATUS=$(printf '%s' "$HEALTH" | jq -r '.backend.status // empty' 2>/dev/null || true)
    CACHED=$(printf '%s' "$HEALTH" | jq -r '.cached_torrents // empty' 2>/dev/null || true)
  else
    STATUS=$(echo "$HEALTH" | grep -o '"status"[[:space:]]*:[[:space:]]*"[^"]*"' | cut -d'"' -f4)
    BACKEND_TYPE=$(echo "$HEALTH" | sed -n 's/.*"backend"[[:space:]]*:[[:space:]]*{[^}]*"type"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    BACKEND_STATUS=$(echo "$HEALTH" | sed -n 's/.*"backend"[[:space:]]*:[[:space:]]*{[^}]*"status"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
    CACHED=$(echo "$HEALTH" | grep -o '"cached_torrents"[[:space:]]*:[[:space:]]*[0-9]*' | tr -dc '0-9')
  fi
  if [ "$STATUS" = "ok" ]; then
    ok "Sidecar health: $STATUS"
  else
    fail "Sidecar health: ${STATUS:-unreachable}"
  fi
  if [ "$BACKEND_STATUS" = "connected" ]; then
    ok "Backend connection: ${BACKEND_TYPE:-unknown} $BACKEND_STATUS"
  else
    fail "Backend connection: ${BACKEND_TYPE:-unknown} ${BACKEND_STATUS:-unknown}"
  fi
  info "Cached torrents: ${CACHED:-unknown}"
else
  info "curl not available - skipping sidecar test"
fi

echo
echo "=== Result: $PASS passed, $FAIL failed ==="
[ "$FAIL" -eq 0 ]
