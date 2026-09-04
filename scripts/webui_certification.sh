#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${TNG_WEBUI_REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-${TNG_WEBUI_REPORT:-$REPORT_DIR/webui-certification-$(date -u +%Y%m%dT%H%M%SZ).md}}"

mkdir -p "$REPORT_DIR"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

run_gate() {
  local name="$1"
  shift
  local log="$tmpdir/$(printf '%s' "$name" | tr -c 'A-Za-z0-9_.-' '_').log"
  if "$@" >"$log" 2>&1; then
    printf '| %s | PASS |\n' "$name" >>"$OUT"
  else
    printf '| %s | FAIL |\n' "$name" >>"$OUT"
    overall=1
  fi
  {
    echo
    echo "## $name"
    echo
    echo '```text'
    cat "$log"
    echo '```'
  } >>"$OUT"
}

overall=0

{
  echo "# TorrentNG WebUI Certification"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- 15k first-visible threshold: ${TNG_WEBUI_FIRST_VISIBLE_MS:-8000}ms"
  echo
  echo "| Gate | Result |"
  echo "| --- | --- |"
} >"$OUT"

run_gate "webui production build" npm --prefix "$ROOT/webui" run build
run_gate "webui lint" npm --prefix "$ROOT/webui" run lint
run_gate "webui browser matrix" npm --prefix "$ROOT/webui" run test:e2e -- --reporter=list

{
  echo
  if [[ "$overall" -eq 0 ]]; then
    echo "Overall status: PASS"
  else
    echo "Overall status: FAIL"
  fi
} >>"$OUT"

echo "$OUT"
exit "$overall"
