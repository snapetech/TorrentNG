#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/certification-burndown-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status_tmp="$(mktemp)"
trap 'rm -f "$status_tmp"' EXIT

"$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$status_tmp"

nonclean_rows() {
  awk -F'|' '
    /^\|/ && $2 !~ /^---/ && $2 !~ /Gate/ {
      name=$2; status=$3; report=$4;
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", name);
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", status);
      gsub(/^[[:space:]]+|[[:space:]]+$/, "", report);
      if (name == "Certification burndown") next;
      if (status != "PASS" && status != "INFO") {
        print name "\t" status "\t" report;
      }
    }
  ' "$status_tmp"
}

action_for() {
  local gate="$1"
  local status="$2"
  case "$gate:$status" in
    "Universal compatibility:PASS_WITH_SKIPS")
      printf 'Run live legs as needed: `UNIVERSAL_COMPAT_LIVE=1 scripts/universal_compatibility_certification.sh`, `UNIVERSAL_COMPAT_PUBLIC=1 scripts/universal_compatibility_certification.sh`, and `UNIVERSAL_COMPAT_REAL_DEVICE=1 scripts/universal_compatibility_certification.sh` after configuring Docker, public-swarm policy, and storage target paths.'
      ;;
    "Migration corpus:PASS_WITH_GAPS")
      printf 'Populate `testdata/migration-corpus/{qbittorrent,transmission,deluge,utorrent,biglybt,tixati,rtorrent,generic}` with real exported artifacts, then run `TNG_REQUIRE_MIGRATION_CORPUS=1 scripts/migration_corpus_certification.sh`.'
      ;;
    "24h soak:STALE/INCOMPLETE")
      printf 'Start a fresh 24h soak with `scripts/soak_certification.sh 24h` or replace the stale requirement in release policy if transfer-churn soak supersedes it.'
      ;;
    "Local release gate:PASS_WITH_WARNINGS")
      printf 'Rerun `scripts/local_release_gate.sh` after warning rows are resolved. Set `TNG_STORAGE_MATRIX_TARGETS` for real-device storage release probes.'
      ;;
    "Post-soak release gate:PASS_WITH_WARNINGS")
      printf 'Rerun `scripts/post_soak_release_gate.sh` after all upstream warning rows are resolved.'
      ;;
    *:MISSING)
      printf 'Generate the missing report for this gate or remove it from `scripts/certification_status.sh` if it is no longer a release requirement.'
      ;;
    *:FAIL)
      printf 'Open the latest report, fix the failing command, and rerun that gate.'
      ;;
    *)
      printf 'Review latest report and either resolve the warning condition or document why it is an accepted non-release claim.'
      ;;
  esac
}

{
  echo "# TorrentNG Certification Burndown"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Report directory: $REPORT_DIR"
  echo
  echo "## Non-Clean Rows"
  echo
  echo "| Gate | Status | Latest report | Action |"
  echo "|---|---|---|---|"
} >"$OUT"

count=0
while IFS=$'\t' read -r gate status report; do
  [[ -n "$gate" ]] || continue
  count=$((count + 1))
  action="$(action_for "$gate" "$status")"
  action="${action//|/\\|}"
  printf '| %s | %s | %s | %s |\n' "$gate" "$status" "$report" "$action" >>"$OUT"
done < <(nonclean_rows)

{
  echo
  echo "Non-clean rows: $count"
  if [[ "$count" -eq 0 ]]; then
    echo
    echo "Overall status: PASS"
  else
    echo
    echo "Overall status: PASS_WITH_ACTIONS"
  fi
} >>"$OUT"

echo "$OUT"
