#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

report_dir="$tmpdir/reports"
benchmark_dir="$tmpdir/benchmarks"
bundle_dir="$tmpdir/bundles"
mkdir -p "$report_dir" "$benchmark_dir" "$bundle_dir"

write_report() {
  local path="$1"
  {
    echo "# selftest"
    echo
    echo "Overall status: PASS"
  } >"$path"
}

for report in \
  live-cert-selftest.md \
  client-config-selftest.md \
  live-transfer-selftest.md \
  release-grab-selftest.md \
  app-add-job-selftest.md \
  arr-app-selftest.md \
  autobrr-selftest.md \
  dht-cert-selftest.md \
  natpmp-dht-selftest.md \
  proton-natpmp-selftest.md \
  proton-tng-dht-selftest.md \
  mobile-compat-selftest.md \
  phase1-cert-selftest.md \
  universal-compat-selftest.md \
  universal-live-selftest.md \
  migration-corpus-selftest.md \
  soak-20260517-selftest.md \
  transfer-churn-selftest.md \
  soak-24h-selftest.md \
  soak-status-selftest.md \
  soak-final-selftest.md \
  security-review-selftest.md \
  security-scan-selftest.md \
  native-engine-selftest.md \
  webui-certification-selftest.md \
  local-release-selftest.md \
  storage-hardware-selftest.md \
  storage-uring-graduation-selftest.md \
  storage-move-import-selftest.md \
  storage-release-certification-selftest.md \
  storage-certification-index.md \
  pre-engine-release-selftest.md \
  pre-engine-suite-selftest.md \
  post-soak-release-selftest.md \
  certification-burndown-selftest.md \
  release-readiness-selftest.md; do
  write_report "$report_dir/$report"
done
write_report "$benchmark_dir/report-selftest.md"

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" CERTIFICATION_BUNDLE_DIR="$bundle_dir" \
  "$ROOT/scripts/certification_bundle.sh" "$bundle_dir/pass.tar.gz" >/dev/null
grep -q '^Overall status: PASS$' "$report_dir/certification-bundle-"*.md
grep -q 'Missing referenced reports: 0' "$report_dir/certification-bundle-"*.md
tar -tzf "$bundle_dir/pass.tar.gz" | grep -q 'benchmarks/report-selftest.md'

rm -f "$report_dir/universal-live-selftest.md"
write_report "$report_dir/universal-live-selftest.md"
sleep 1
REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" CERTIFICATION_BUNDLE_DIR="$bundle_dir" \
  TNG_CERT_BUNDLE_TEST_REMOVE_REPORT=universal-live-selftest.md \
  "$ROOT/scripts/certification_bundle.sh" "$bundle_dir/warn.tar.gz" >/dev/null || true
latest="$(ls -t "$report_dir"/certification-bundle-*.md | head -1)"
grep -q 'Overall status: PASS_WITH_WARNINGS' "$latest"
grep -q 'Missing referenced reports: 1' "$latest"
tar -xOzf "$bundle_dir/warn.tar.gz" "$(tar -tzf "$bundle_dir/warn.tar.gz" | grep '/MANIFEST.md$')" \
  | grep -q 'universal-live-selftest.md | missing at bundle time'

echo "certification bundle self-test: PASS"
