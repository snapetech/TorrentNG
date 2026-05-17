#!/usr/bin/env bash
set -euo pipefail

TNG_CONTAINER="${TNG_CONTAINER:-certification-torrentng-1}"
COUNT="${TNG_SEED_TORRENTS:-15000}"
DB_PATH="${TNG_CACHE_DB_PATH:-/var/lib/torrentng/cache.db}"
CATEGORY="${TNG_SEED_CATEGORY:-cert-scale}"
BASE_UPDATED_AT="${TNG_SEED_BASE_UPDATED_AT:-$(date +%s)}"

docker exec -i "$TNG_CONTAINER" sh -s "$COUNT" "$DB_PATH" "$CATEGORY" "$BASE_UPDATED_AT" <<'SH'
set -euo pipefail
count="$1"
db="$2"
category="$3"
base_updated_at="$4"

if ! command -v sqlite3 >/dev/null 2>&1; then
  apk add --no-cache sqlite >/dev/null
fi

sqlite3 "$db" <<SQL
PRAGMA journal_mode=WAL;
PRAGMA synchronous=NORMAL;
BEGIN;
DELETE FROM torrents WHERE category = '$category';
WITH RECURSIVE seq(i) AS (
  SELECT 0
  UNION ALL
  SELECT i + 1 FROM seq WHERE i + 1 < $count
)
INSERT OR REPLACE INTO torrents (
  hash, name, size_bytes, bytes_done, down_rate, up_rate,
  up_total, down_total, ratio, is_active, is_open, complete,
  state, priority, category, base_path, directory, creation_date,
  timestamp_finished, tracker_focus, peers_connected, peers_complete,
  message, tracker_url, tags, updated_at
)
SELECT
  printf('cert-scale-%08x', i),
  printf('Certification Scale Torrent %08d', i),
  1000000000 + i,
  CASE WHEN i % 3 = 0 THEN 1000000000 + i ELSE (1000000000 + i) / 2 END,
  CASE WHEN i % 7 = 0 THEN 1024 + i ELSE 0 END,
  CASE WHEN i % 5 = 0 THEN 2048 + i ELSE 0 END,
  i * 100,
  i * 50,
  1000,
  CASE WHEN i % 4 = 0 THEN 1 ELSE 0 END,
  CASE WHEN i % 2 = 0 THEN 1 ELSE 0 END,
  CASE WHEN i % 3 = 0 THEN 1 ELSE 0 END,
  0,
  0,
  '$category',
  '/data',
  printf('/data/cert-scale/%08d', i),
  i,
  0,
  0,
  i % 20,
  i % 10,
  '',
  'udp://tracker.example/announce',
  '',
  $base_updated_at + i
FROM seq
;
COMMIT;
SQL

sqlite3 "$db" "SELECT COUNT(*) FROM torrents WHERE category = '$category';"
SH
