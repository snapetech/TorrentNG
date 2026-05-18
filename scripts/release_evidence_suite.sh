#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/release-evidence-suite-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status="PASS"

run_capture() {
  local name="$1"
  shift
  local result="PASS"
  {
    echo
    echo "## $name"
    echo
    echo "- Command: \`$*\`"
    echo
    echo '```text'
  } >>"$OUT"
  if (cd "$ROOT" && "$@") >>"$OUT" 2>&1; then
    result="PASS"
  else
    result="FAIL"
    status="FAIL"
  fi
  echo '```' >>"$OUT"
  printf '| %s | %s |\n' "$name" "$result" >>"$OUT.table"
}

{
  echo "# TorrentNG Release Evidence Suite"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Report directory: $REPORT_DIR"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result |"
  echo "|---|---|"
} >"$OUT"
: >"$OUT.table"

run_capture "certification status" "$ROOT/scripts/certification_status.sh" "$REPORT_DIR"
run_capture "certification JSON status" "$ROOT/scripts/certification_status_json.sh"
run_capture "external evidence preflight" "$ROOT/scripts/external_evidence_preflight.sh"
run_capture "certification burndown" "$ROOT/scripts/certification_burndown.sh"
run_capture "strict release readiness" "$ROOT/scripts/release_readiness_gate.sh"
run_capture "certification bundle" "$ROOT/scripts/certification_bundle.sh"

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
