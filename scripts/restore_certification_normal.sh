#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="${CERT_COMPOSE_FILE:-$ROOT/deploy/certification/compose.yml}"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"

TNG_SYNC_INTERVAL_SECS=2 \
TNG_HOST_PORT="${TNG_HOST_PORT:-28080}" \
TNG_INCOMING_PORT="${TNG_INCOMING_PORT:-51000}" \
SONARR_HOST_PORT="${SONARR_HOST_PORT:-28989}" \
RADARR_HOST_PORT="${RADARR_HOST_PORT:-27878}" \
PROWLARR_HOST_PORT="${PROWLARR_HOST_PORT:-29696}" \
AUTOBRR_HOST_PORT="${AUTOBRR_HOST_PORT:-27474}" \
CROSS_SEED_HOST_PORT="${CROSS_SEED_HOST_PORT:-22468}" \
docker compose --env-file "$ENV_FILE" -f "$COMPOSE_FILE" up -d torrentng

echo "TorrentNG certification service restored with TNG_SYNC_INTERVAL_SECS=2"
echo "URL: http://localhost:${TNG_HOST_PORT:-28080}"
