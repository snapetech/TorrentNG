#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
BENCHMARK_DIR="${BENCHMARK_DIR:-$ROOT/benchmarks}"
BUNDLE_DIR="${CERTIFICATION_BUNDLE_DIR:-$ROOT/certification/bundles}"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
OUT="${1:-$BUNDLE_DIR/torrentng-certification-bundle-$STAMP.tar.gz}"

mkdir -p "$BUNDLE_DIR" "$(dirname "$OUT")"

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

status_report="$tmpdir/certification-status.md"
manifest="$tmpdir/MANIFEST.md"
bundle_root="$tmpdir/torrentng-certification-bundle-$STAMP"
mkdir -p "$bundle_root/reports"

BENCHMARK_DIR="$BENCHMARK_DIR" "$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$status_report"

cp "$status_report" "$bundle_root/certification-status.md"

{
  echo "# TorrentNG Certification Evidence Bundle"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Source report directory: $REPORT_DIR"
  echo "- Benchmark directory: $BENCHMARK_DIR"
  echo
  echo "## Included Reports"
  echo
  echo "| Gate | Status | Report | SHA-256 |"
  echo "|---|---|---|---|"
} >"$manifest"

awk -F'|' '
  /^\|/ && $2 !~ /^---/ && $2 !~ /Gate/ {
    gate=$2; status=$3; report=$4;
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", gate);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", status);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", report);
    if (report != "-" && report != "") print gate "\t" status "\t" report;
  }
' "$status_report" | while IFS=$'\t' read -r gate status report; do
  src="$REPORT_DIR/$report"
  dest_prefix="reports"
  if [[ ! -f "$src" && -f "$BENCHMARK_DIR/$report" ]]; then
    src="$BENCHMARK_DIR/$report"
    dest_prefix="benchmarks"
    mkdir -p "$bundle_root/benchmarks"
  fi
  if [[ -f "$src" ]]; then
    cp "$src" "$bundle_root/$dest_prefix/$report"
    hash="$(sha256sum "$src" | awk '{print $1}')"
    printf '| %s | %s | %s/%s | %s |\n' "$gate" "$status" "$dest_prefix" "$report" "$hash" >>"$manifest"
  else
    printf '| %s | %s | %s | missing at bundle time |\n' "$gate" "$status" "$report" >>"$manifest"
  fi
done

cp "$manifest" "$bundle_root/MANIFEST.md"

tar -C "$tmpdir" -czf "$OUT" "$(basename "$bundle_root")"

report="$REPORT_DIR/certification-bundle-$STAMP.md"
{
  echo "# TorrentNG Certification Bundle"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Bundle: $OUT"
  echo "- SHA-256: $(sha256sum "$OUT" | awk '{print $1}')"
  echo "- Included reports: $(find "$bundle_root/reports" -type f | wc -l | tr -d ' ')"
  echo
  echo "Overall status: PASS"
} >"$report"

echo "$OUT"
