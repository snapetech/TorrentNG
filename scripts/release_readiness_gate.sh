#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/release-readiness-$(date -u +%Y%m%dT%H%M%SZ).md}"
RELEASE_SCOPE="${TNG_RELEASE_SCOPE:-strict}"

mkdir -p "$(dirname "$OUT")"

status_report="$(mktemp)"
burndown_report="$REPORT_DIR/release-readiness-burndown-$(date -u +%Y%m%dT%H%M%SZ).md"
trap 'rm -f "$status_report"' EXIT

"$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$status_report"
"$ROOT/scripts/certification_burndown.sh" "$burndown_report" >/dev/null

nonclean="$(
  awk -F'|' -v scope="$RELEASE_SCOPE" '
    function local_scope_ignored(name, status) {
      if (scope != "local") return 0;
      if (name == "Universal compatibility" && status == "PASS_WITH_SKIPS") return 1;
      if (name == "Universal live compatibility" && status == "PASS_WITH_SKIPS") return 1;
      if (name == "External evidence preflight") return 1;
      if (name == "24h soak" && status == "STALE/INCOMPLETE") return 1;
      if (name == "Local release gate" && status == "PASS_WITH_WARNINGS") return 1;
      if (name == "Post-soak release gate" && status == "PASS_WITH_WARNINGS") return 1;
      return 0;
    }
    /^\|/ && $2 !~ /^---/ && $2 !~ /Gate/ {
      name=$2; status=$3; report=$4;
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", name);
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status);
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", report);
      if (name == "Certification burndown") next;
      if (name == "Release readiness") next;
      if (name == "Certification bundle") next;
      if (name == "Release evidence suite") next;
      if (name == "Certification JSON status") next;
      if (local_scope_ignored(name, status)) next;
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
  echo "- Scope: $RELEASE_SCOPE"
  if [[ "$RELEASE_SCOPE" == "local" ]]; then
    echo "- Scope policy: external opt-in evidence rows are documented but not release-blocking for this local readiness run"
  fi
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
