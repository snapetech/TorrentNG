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
      if (name == "Release readiness") next;
      if (name == "Certification bundle") next;
      if (name == "Release evidence suite") next;
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
      printf 'Run `scripts/universal_live_certification.sh` for Docker local interop. Add `UNIVERSAL_LIVE_PUBLIC=1` for approved public torrent downloads and `UNIVERSAL_LIVE_REAL_DEVICE=1 TNG_STORAGE_BENCH_DIR=/mnt/target` for target storage hardware.'
      ;;
    "Universal live compatibility:MISSING")
      printf 'Run `scripts/universal_live_certification.sh` for Docker local interop, or set `UNIVERSAL_LIVE_PUBLIC=1` / `UNIVERSAL_LIVE_REAL_DEVICE=1` for the external legs needed by the release.'
      ;;
    "Universal live compatibility:PASS_WITH_SKIPS")
      printf 'Rerun `scripts/universal_live_certification.sh` with the skipped live legs enabled: `UNIVERSAL_LIVE_PUBLIC=1` for public torrents and/or `UNIVERSAL_LIVE_REAL_DEVICE=1 TNG_STORAGE_BENCH_DIR=/mnt/target` for storage hardware.'
      ;;
    "Migration corpus:PASS_WITH_GAPS")
      printf 'Populate `testdata/migration-corpus/{qbittorrent,transmission,deluge,utorrent,biglybt,tixati,rtorrent,generic}` with real exported artifacts, then run `TNG_REQUIRE_MIGRATION_CORPUS=1 scripts/migration_corpus_certification.sh`.'
      ;;
    "External evidence preflight:PASS_WITH_WARNINGS")
      printf 'Open the latest external preflight report, satisfy WARN rows, then rerun `TNG_EXTERNAL_PREFLIGHT_STRICT=1 scripts/external_evidence_preflight.sh` before starting long live/corpus/soak gates.'
      ;;
    "24h soak:STALE/INCOMPLETE")
      printf 'Start a fresh 24h soak with `scripts/start_24h_soak.sh`, monitor it with `scripts/soak_status.sh`, then finalize with `SOAK_MIN_SAMPLES=1200 RESTORE_NORMAL=1 scripts/finalize_soak.sh <report>`. If transfer-churn soak supersedes this release requirement, remove the stale 24h row from release policy instead.'
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
