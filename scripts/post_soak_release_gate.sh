#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT="${1:-}"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${2:-$REPORT_DIR/post-soak-release-$(date -u +%Y%m%dT%H%M%SZ).md}"

if [[ -z "$REPORT" ]]; then
  REPORT="$(find "$REPORT_DIR" -maxdepth 1 -type f -name 'soak-24h-*.md' -printf '%T@ %p\n' 2>/dev/null | sort -nr | awk 'NR==1 {print $2}')"
fi

mkdir -p "$(dirname "$OUT")"
status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  if [[ "$result" != "PASS" && "$result" != "INFO" ]]; then
    status="FAIL"
  fi
}

run_gate() {
  local name="$1"
  local report="$2"
  shift 2
  if "$@" "$report"; then
    mark "$name" "PASS" "$report"
  else
    mark "$name" "FAIL" "$report"
  fi
}

{
  echo "# rtorrentNG Post-Soak Release Gate"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Source soak report: ${REPORT:-missing}"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

run_gate "soak status" "$REPORT_DIR/soak-status-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/soak_status.sh" "$REPORT"
RESTORE_NORMAL=1 run_gate "soak finalization and restore" "$REPORT_DIR/soak-final-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/finalize_soak.sh" "$REPORT"
run_gate "native engine rewrite certification" "$REPORT_DIR/native-engine-post-soak-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/native_engine_certification_report.sh"
run_gate "short certification suite" "$REPORT_DIR/pre-engine-suite-post-soak-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/pre_engine_certification_suite.sh"
run_gate "public legal torrent interop matrix" "$REPORT_DIR/interop-public-post-soak-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/interop_matrix.sh" --public
run_gate "release report refresh" "$REPORT_DIR/pre-engine-release-post-soak-$(date -u +%Y%m%dT%H%M%SZ).md" "$ROOT/scripts/pre_engine_release_report.sh"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
