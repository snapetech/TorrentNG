#!/bin/sh
set -eu

: "${GLUETUN_CONTROL_API_KEY:?GLUETUN_CONTROL_API_KEY is required}"
: "${TORRENTNGD_API_TOKEN:?TORRENTNGD_API_TOKEN is required}"

interval=${VPN_PORT_SYNC_INTERVAL:-10}
case "$interval" in
  ''|*[!0-9]*) echo "VPN_PORT_SYNC_INTERVAL must be a positive integer" >&2; exit 2 ;;
esac
if [ "$interval" -lt 1 ]; then
  echo "VPN_PORT_SYNC_INTERVAL must be a positive integer" >&2
  exit 2
fi

while :; do
  forwarded_response=$(curl -fsS --max-time 5 \
    -H "X-API-Key: ${GLUETUN_CONTROL_API_KEY}" \
    http://127.0.0.1:8000/v1/portforward 2>/dev/null) || {
    sleep "$interval"
    continue
  }
  forwarded_port=$(printf '%s' "$forwarded_response" | jq -er \
    '.port | select(type == "number" and . == floor and . >= 1 and . <= 65535)' 2>/dev/null) || {
    sleep "$interval"
    continue
  }

  settings_response=$(curl -fsS --max-time 5 \
    -H "Authorization: Bearer ${TORRENTNGD_API_TOKEN}" \
    http://127.0.0.1:8080/api/v1/session/settings 2>/dev/null) || {
    sleep "$interval"
    continue
  }
  current_port=$(printf '%s' "$settings_response" | jq -er \
    '.listen_port | select(type == "number" and . == floor and . >= 1 and . <= 65535)' 2>/dev/null) || {
    sleep "$interval"
    continue
  }

  if [ "$forwarded_port" != "$current_port" ]; then
    if curl -fsS --max-time 15 -o /dev/null \
      -X PATCH \
      -H "Authorization: Bearer ${TORRENTNGD_API_TOKEN}" \
      -H 'Content-Type: application/json' \
      --data "{\"listen_port\":${forwarded_port}}" \
      http://127.0.0.1:8080/api/v1/session/settings; then
      printf 'TorrentNG peer port updated to %s\n' "$forwarded_port"
    else
      printf 'TorrentNG rejected forwarded peer port %s; retrying\n' "$forwarded_port" >&2
    fi
  fi
  sleep "$interval"
done
