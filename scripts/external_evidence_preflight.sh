#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/external-evidence-preflight-$(date -u +%Y%m%dT%H%M%SZ).md}"
CORPUS_DIR="${TNG_MIGRATION_CORPUS_DIR:-$ROOT/testdata/migration-corpus}"
STORAGE_TARGET="${TNG_STORAGE_BENCH_DIR:-}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
warnings=0

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >>"$OUT"
  case "$result" in
    PASS|INFO) ;;
    WARN) warnings=$((warnings + 1)) ;;
    *) status="FAIL" ;;
  esac
}

corpus_missing=0
for family in qbittorrent transmission deluge utorrent biglybt tixati rtorrent generic; do
  dir="$CORPUS_DIR/$family"
  if [[ ! -d "$dir" ]] || [[ -z "$(find "$dir" -type f 2>/dev/null | head -1)" ]]; then
    corpus_missing=$((corpus_missing + 1))
  fi
done

{
  echo "# TorrentNG External Evidence Preflight"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Corpus directory: $CORPUS_DIR"
  echo "- Storage target: ${STORAGE_TARGET:-unset}"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} >"$OUT"

if command -v docker >/dev/null 2>&1; then
  if docker info >/dev/null 2>&1; then
    mark "Docker daemon" "PASS" "reachable"
  else
    mark "Docker daemon" "WARN" "docker command exists but daemon is unavailable to current user"
  fi
else
  mark "Docker daemon" "WARN" "docker command missing; live interop cannot run"
fi

if [[ "${UNIVERSAL_LIVE_PUBLIC:-0}" == "1" ]]; then
  mark "public torrent opt-in" "PASS" "UNIVERSAL_LIVE_PUBLIC=1"
else
  mark "public torrent opt-in" "WARN" "set UNIVERSAL_LIVE_PUBLIC=1 only after approving public legal torrent downloads"
fi

if [[ -n "$STORAGE_TARGET" && -d "$STORAGE_TARGET" && -w "$STORAGE_TARGET" ]]; then
  mark "real-device storage target" "PASS" "$STORAGE_TARGET is writable"
else
  mark "real-device storage target" "WARN" "set TNG_STORAGE_BENCH_DIR to a writable target mount"
fi

if [[ "$corpus_missing" -eq 0 ]]; then
  mark "migration corpus coverage" "PASS" "all source-family directories contain files"
else
  mark "migration corpus coverage" "WARN" "$corpus_missing source-family directories are missing evidence files"
fi

if pgrep -af 'soak_certification.sh' | grep -q 'soak-24h-'; then
  mark "24h soak active" "PASS" "$(pgrep -af 'soak_certification.sh' | grep 'soak-24h-' | head -1)"
else
  mark "24h soak active" "WARN" "no active soak-24h run detected; use scripts/start_24h_soak.sh"
fi

{
  echo
  if [[ "$status" == "PASS" && "$warnings" -gt 0 ]]; then
    echo "Overall status: PASS_WITH_WARNINGS"
    echo "Warnings: $warnings"
  else
    echo "Overall status: $status"
  fi
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
