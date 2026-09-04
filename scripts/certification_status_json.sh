#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
BENCHMARK_DIR="${BENCHMARK_DIR:-$ROOT/benchmarks}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${1:-$REPORT_DIR/certification-status-$STAMP.json}"
REPORT="${OUT%.json}.md"

mkdir -p "$(dirname "$OUT")"

tmp_status="$(mktemp)"
tmp_rows="$(mktemp)"
trap 'rm -f "$tmp_status" "$tmp_rows"' EXIT

BENCHMARK_DIR="$BENCHMARK_DIR" "$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$tmp_status"

awk -F'|' '
  /^\|/ && $2 !~ /^---/ && $2 !~ /Gate/ {
    gate=$2; status=$3; report=$4;
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", gate);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", status);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", report);
    print gate "\t" status "\t" report;
  }
' "$tmp_status" >"$tmp_rows"

json_escape() {
  sed \
    -e 's/\\/\\\\/g' \
    -e 's/"/\\"/g' \
    -e 's/	/\\t/g'
}

json_value() {
  printf '%s' "$1" | json_escape
}

pass=0
info=0
warn=0
fail=0
missing=0
other=0
total=0

while IFS=$'\t' read -r _gate status _report; do
  [[ -n "$status" ]] || continue
  total=$((total + 1))
  case "$status" in
    PASS) pass=$((pass + 1)) ;;
    INFO) info=$((info + 1)) ;;
    PASS_WITH_*|WARN|SKIP|STALE/INCOMPLETE|RUNNING|RUNNING/UNKNOWN) warn=$((warn + 1)) ;;
    FAIL) fail=$((fail + 1)) ;;
    MISSING) missing=$((missing + 1)) ;;
    *) other=$((other + 1)) ;;
  esac
done <"$tmp_rows"

if [[ "$fail" -gt 0 || "$missing" -gt 0 || "$other" -gt 0 ]]; then
  overall_status="FAIL"
elif [[ "$warn" -gt 0 ]]; then
  overall_status="PASS_WITH_WARNINGS"
else
  overall_status="PASS"
fi

{
  echo "{"
  printf '  "generated_utc": "%s",\n' "$(date -u +%Y-%m-%dT%H:%M:%SZ)"
  printf '  "commit": "%s",\n' "$(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  printf '  "report_dir": "%s",\n' "$(json_value "$REPORT_DIR")"
  printf '  "benchmark_dir": "%s",\n' "$(json_value "$BENCHMARK_DIR")"
  echo '  "summary": {'
  printf '    "total": %s,\n' "$total"
  printf '    "pass": %s,\n' "$pass"
  printf '    "info": %s,\n' "$info"
  printf '    "warn": %s,\n' "$warn"
  printf '    "fail": %s,\n' "$fail"
  printf '    "missing": %s,\n' "$missing"
  printf '    "other": %s\n' "$other"
  echo '  },'
  echo '  "gates": ['
  first=1
  while IFS=$'\t' read -r gate status report; do
    [[ -n "$gate" ]] || continue
    if [[ "$first" == "1" ]]; then
      first=0
    else
      echo ","
    fi
    printf '    {"gate": "%s", "status": "%s", "latest_report": "%s"}' \
      "$(json_value "$gate")" "$(json_value "$status")" "$(json_value "$report")"
  done <"$tmp_rows"
  echo
  echo '  ]'
  echo "}"
} >"$OUT"

{
  echo "# TorrentNG Certification JSON Status"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- JSON: $OUT"
  echo "- JSON SHA-256: $(sha256sum "$OUT" | awk '{print $1}')"
  echo "- Gates: $total"
  echo "- Warnings: $warn"
  echo "- Failures: $fail"
  echo "- Missing: $missing"
  echo
  echo "Overall status: $overall_status"
} >"$REPORT"

echo "$OUT"
[[ "$overall_status" != "FAIL" ]]
