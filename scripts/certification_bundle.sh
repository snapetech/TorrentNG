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
missing=0

BENCHMARK_DIR="$BENCHMARK_DIR" "$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$status_report"

if [[ -n "${TNG_CERT_BUNDLE_TEST_REMOVE_REPORT:-}" ]]; then
  rm -f "$REPORT_DIR/$TNG_CERT_BUNDLE_TEST_REMOVE_REPORT" "$BENCHMARK_DIR/$TNG_CERT_BUNDLE_TEST_REMOVE_REPORT"
fi

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

while IFS=$'\t' read -r gate status report; do
  [[ -n "$gate" ]] || continue
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
    missing=$((missing + 1))
    printf '| %s | %s | %s | missing at bundle time |\n' "$gate" "$status" "$report" >>"$manifest"
  fi
done < <(awk -F'|' '
  /^\|/ && $2 !~ /^---/ && $2 !~ /Gate/ {
    gate=$2; status=$3; report=$4;
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", gate);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", status);
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", report);
    if (report != "-" && report != "") print gate "\t" status "\t" report;
  }
' "$status_report")

cp "$manifest" "$bundle_root/MANIFEST.md"
status_hash="$(sha256sum "$bundle_root/certification-status.md" | awk '{print $1}')"
manifest_hash="$(sha256sum "$bundle_root/MANIFEST.md" | awk '{print $1}')"

tar -C "$tmpdir" -czf "$OUT" "$(basename "$bundle_root")"
bundle_hash="$(sha256sum "$OUT" | awk '{print $1}')"
included_reports="$(find "$bundle_root/reports" "$bundle_root/benchmarks" -type f 2>/dev/null | wc -l | tr -d ' ')"
if [[ "$missing" -gt 0 ]]; then
  status="PASS_WITH_WARNINGS"
else
  status="PASS"
fi

report="$REPORT_DIR/certification-bundle-$STAMP.md"
{
  echo "# TorrentNG Certification Bundle"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Bundle: $OUT"
  echo "- Bundle SHA-256: $bundle_hash"
  echo "- Manifest SHA-256: $manifest_hash"
  echo "- Certification status SHA-256: $status_hash"
  echo "- Included reports: $included_reports"
  echo "- Missing referenced reports: $missing"
  echo
  echo "Overall status: $status"
} >"$report"

echo "$OUT"
[[ "$status" == "PASS" ]]
