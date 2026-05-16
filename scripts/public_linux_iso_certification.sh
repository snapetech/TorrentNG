#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/public-linux-iso-$(date -u +%Y%m%dT%H%M%SZ).md}"

export PUBLIC_TRANSFER=1
export PUBLIC_TORRENT_URL="${PUBLIC_TORRENT_URL:-https://mirror.arizona.edu/debian-cd/current/amd64/bt-cd/debian-13.4.0-amd64-netinst.iso.torrent}"

"$ROOT/scripts/live_transfer_certification.sh" "$OUT"

