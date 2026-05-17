#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/release-readiness-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status_report="$(mktemp)"
burndown_report="$REPORT_DIR/release-readiness-burndown-$(date -u +%Y%m%dT%H%M%SZ).md"
trap 'rm -f "$status_report"' EXIT

"$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$status_report"
"$ROOT/scripts/certification_burndown.sh" "$burndown_report" >/dev/null

nonclean="$(
  awk -F'|' '
    /^\|/ && $2 !~ /^---/ && $2 !~ /Gate/ {
      name=$2; status=$3; report=$4;
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", name);
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status);
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", report);
      if (name == "Certification burndown") next;
      if (name == "Release readiness") next;
      if (name == "Certification bundle") next;
      if (name == "Release evidence suite") next;
      if (status != "PASS" && status != "INFO") {
        print "| " name " | " status " | " report " |";
      }
    }
  ' "$status_report"
)"

{
  echo "# TorrentNG Release Readiness Gate"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Report directory: $REPORT_DIR"
  echo "- Burndown report: $(basename "$burndown_report")"
  echo
  echo "## Non-Clean Certification Rows"
  echo
  echo "| Gate | Status | Latest report |"
  echo "|---|---|---|"
  if [[ -n "$nonclean" ]]; then
    printf '%s\n' "$nonclean"
  else
    echo "| none | PASS | - |"
  fi
  echo
  if [[ -n "$nonclean" ]]; then
    echo "Overall status: FAIL"
  else
    echo "Overall status: PASS"
  fi
} >"$OUT"

echo "$OUT"
[[ -z "$nonclean" ]]
