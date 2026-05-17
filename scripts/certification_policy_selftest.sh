#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

report_dir="$tmpdir/reports"
benchmark_dir="$tmpdir/benchmarks"
mkdir -p "$report_dir" "$benchmark_dir"

write_report() {
  local path="$1"
  local status="$2"
  {
    echo "# selftest"
    echo
    echo "Overall status: $status"
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
  external-evidence-preflight-selftest.md \
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
  pre-engine-release-selftest.md \
  pre-engine-suite-selftest.md \
  post-soak-release-selftest.md; do
  write_report "$report_dir/$report" PASS
done
write_report "$benchmark_dir/report-selftest.md" PASS

cat >"$report_dir/storage-hardware-selftest.md" <<'REPORT'
# TorrentNG Storage Hardware Matrix

Overall status: PASS
REPORT

cat >"$report_dir/storage-uring-graduation-selftest.md" <<'REPORT'
# TorrentNG io_uring Graduation Report

## uring

- Result: PASS
- Selected: uring
- Fixed-buffer strategy: frame_pool_slots

Overall status: PASS
REPORT

cat >"$report_dir/storage-move-import-selftest.md" <<'REPORT'
# TorrentNG Storage Move/Import Certification

Overall status: PASS
REPORT

write_report "$report_dir/storage-release-certification-selftest.md" PASS
TNG_STORAGE_REPORT_DIR="$report_dir" \
  TNG_STORAGE_REPORT_INDEX="$report_dir/storage-certification-index.md" \
  "$ROOT/scripts/storage_certification_index.sh" >/dev/null
cat >"$report_dir/memory-roadmap-certification-selftest.md" <<'REPORT'
# TorrentNG Memory Roadmap Certification

| Roadmap item | Status | Evidence |
| --- | --- | --- |
| policy selftest | PASS | generated |

Overall status: PASS
REPORT

write_report "$report_dir/certification-burndown-selftest.md" PASS_WITH_ACTIONS
write_report "$report_dir/release-readiness-selftest.md" FAIL
write_report "$report_dir/certification-bundle-selftest.md" PASS_WITH_WARNINGS
write_report "$report_dir/release-evidence-suite-selftest.md" FAIL

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/certification_burndown.sh" "$report_dir/certification-burndown-policy.md" >/dev/null
grep -q 'Non-clean rows: 0' "$report_dir/certification-burndown-policy.md"
grep -q 'Overall status: PASS' "$report_dir/certification-burndown-policy.md"

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/release_readiness_gate.sh" "$report_dir/release-readiness-policy.md" >/dev/null
grep -q '| none | PASS | - |' "$report_dir/release-readiness-policy.md"
grep -q 'Overall status: PASS' "$report_dir/release-readiness-policy.md"

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/post_soak_release_gate.sh" "$report_dir/post-soak-policy.md" >/dev/null
grep -q 'certification status rollup | PASS' "$report_dir/post-soak-policy.md"
grep -q 'Overall status: PASS' "$report_dir/post-soak-policy.md"

if TNG_EXTERNAL_PREFLIGHT_STRICT=1 \
  TNG_MIGRATION_CORPUS_DIR="$tmpdir/missing-corpus" \
  "$ROOT/scripts/external_evidence_preflight.sh" "$report_dir/external-preflight-strict.md" >/dev/null 2>&1; then
  echo "strict external preflight accepted missing external evidence" >&2
  exit 1
fi
grep -q 'Overall status: FAIL' "$report_dir/external-preflight-strict.md"
grep -q 'Warnings promoted to failures by TNG_EXTERNAL_PREFLIGHT_STRICT=1' "$report_dir/external-preflight-strict.md"

echo "certification policy self-test: PASS"
