#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/external-evidence-preflight-$(date -u +%Y%m%dT%H%M%SZ).md}"
CORPUS_DIR="${TNG_MIGRATION_CORPUS_DIR:-$ROOT/testdata/migration-corpus}"
CORPUS_MANIFEST="$CORPUS_DIR/manifest.toml"
STORAGE_TARGET="${TNG_STORAGE_BENCH_DIR:-}"
STRICT="${TNG_EXTERNAL_PREFLIGHT_STRICT:-0}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
warnings=0

evidence_patterns=(
  "*.torrent"
  "*.fastresume"
  "*.resume"
  "*.resume.json"
  "*.state"
  "*.dat"
  "*.conf"
  "*.config"
  "resume.dat"
  "downloads.config"
  "torrents.config"
)

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >>"$OUT"
  case "$result" in
    PASS|INFO) ;;
    WARN)
      warnings=$((warnings + 1))
      if [[ "$STRICT" == "1" ]]; then
        status="FAIL"
      fi
      ;;
    *) status="FAIL" ;;
  esac
}

count_corpus_evidence() {
  local dir="$1"
  local pattern
  local expr=()

  [[ -d "$dir" ]] || {
    printf '0'
    return
  }

  for pattern in "${evidence_patterns[@]}"; do
    if [[ "${#expr[@]}" -gt 0 ]]; then
      expr+=(-o)
    fi
    expr+=(-name "$pattern")
  done

  find "$dir" -type f \( "${expr[@]}" \) | wc -l | tr -d ' '
}

corpus_missing=0
missing_families=()
for family in qbittorrent transmission deluge utorrent biglybt tixati rtorrent generic; do
  dir="$CORPUS_DIR/$family"
  files="$(count_corpus_evidence "$dir")"
  if [[ "$files" -eq 0 ]]; then
    corpus_missing=$((corpus_missing + 1))
    missing_families+=("$family")
  fi
done

{
  echo "# TorrentNG External Evidence Preflight"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Corpus directory: $CORPUS_DIR"
  echo "- Storage target: ${STORAGE_TARGET:-unset}"
  echo "- Strict mode: $STRICT"
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
  mark "migration corpus coverage" "PASS" "all source-family directories contain migration evidence files"
else
  missing_csv="$(IFS=,; printf '%s' "${missing_families[*]}")"
  mark "migration corpus coverage" "WARN" "$corpus_missing source-family directories are missing evidence files: $missing_csv"
fi

if [[ -f "$CORPUS_MANIFEST" ]]; then
  manifest_report="$REPORT_DIR/external-preflight-migration-corpus-$(date -u +%Y%m%dT%H%M%SZ).md"
  if TNG_MIGRATION_CORPUS_DIR="$CORPUS_DIR" TNG_REQUIRE_MIGRATION_CORPUS=1 \
    "$ROOT/scripts/migration_corpus_certification.sh" "$manifest_report" >/dev/null; then
    mark "migration corpus manifest" "PASS" "validated by $manifest_report"
  else
    mark "migration corpus manifest" "WARN" "manifest exists but validation failed; see $manifest_report"
  fi
else
  mark "migration corpus manifest" "WARN" "manifest.toml missing; copy manifest.example.toml and record artifact provenance"
fi

SOAK_PID_FILE="${TNG_24H_SOAK_PID_FILE:-$ROOT/.run/soak-24h.pid}"
soak_process="$(pgrep -af '[s]oak_certification.sh' | grep 'soak-24h-' | head -1 || true)"
if [[ -z "$soak_process" && -f "$SOAK_PID_FILE" ]]; then
  soak_pid="$(cat "$SOAK_PID_FILE" 2>/dev/null || true)"
  if [[ "$soak_pid" =~ ^[0-9]+$ ]]; then
    soak_process="$(ps -p "$soak_pid" -o args= 2>/dev/null | grep 'soak_certification.sh' | grep 'soak-24h-' || true)"
  fi
fi
if [[ -n "$soak_process" ]]; then
  mark "24h soak active" "PASS" "$soak_process"
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
    if [[ "$status" == "FAIL" && "$STRICT" == "1" ]]; then
      echo "Warnings promoted to failures by TNG_EXTERNAL_PREFLIGHT_STRICT=1"
    fi
  fi
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
