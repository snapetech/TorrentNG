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
  backup-restore-selftest.md \
  backend-burndown-native-release-smoke-selftest.md \
  backend-burndown-fault-matrix-selftest.md \
  backend-api-load-selftest.md \
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
write_report "$report_dir/certification-status-selftest.md" FAIL

if BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/certification_status_json.sh" "$report_dir/status-json-selftest.json" >/dev/null 2>&1; then
  echo "certification status JSON selftest accepted failed gates" >&2
  exit 1
fi
grep -q '"fail":' "$report_dir/status-json-selftest.json"
grep -q 'Overall status: FAIL' "$report_dir/status-json-selftest.md"

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

awk '
  /^PROTOCOL_CASES=\(/ { in_cases=1; next }
  in_cases && /^\)/ { in_cases=0; next }
  in_cases {
    gsub(/^[[:space:]]+|[[:space:]]+$/, "", $0)
    if ($0 != "") print $0
  }
' "$ROOT/scripts/interop_matrix.sh" >"$tmpdir/protocol-cases.txt"
while IFS= read -r protocol_case; do
  grep -q "| \`$protocol_case\` |" "$ROOT/docs/INTEROP_MATRIX.md" || {
    echo "protocol case $protocol_case is missing from docs/INTEROP_MATRIX.md" >&2
    exit 1
  }
done <"$tmpdir/protocol-cases.txt"

write_report "$report_dir/migration-corpus-20260518T000000Z.md" PASS_WITH_GAPS
write_report "$report_dir/migration-corpus-local-release-20260518T999999Z.md" PASS_WITH_WARNINGS
write_report "$report_dir/migration-corpus-universal-20260518T999999Z.md" PASS
BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$report_dir" >"$report_dir/status-migration-selector.md"
grep -q '| Migration corpus | PASS_WITH_GAPS | migration-corpus-20260518T000000Z.md |' "$report_dir/status-migration-selector.md"
write_report "$report_dir/migration-corpus-selftest.md" PASS

TNG_LOCAL_RELEASE_SELFTEST=1 \
  TNG_LOCAL_RELEASE_SELFTEST_REPORT_STATUS=PASS_WITH_WARNINGS \
  env -u TNG_STORAGE_MATRIX_TARGETS \
  "$ROOT/scripts/local_release_gate.sh" "$report_dir/local-release-warning-selftest.md" >/dev/null
grep -q '| migration exported corpus coverage | WARN |' "$report_dir/local-release-warning-selftest.md"
grep -q 'Overall status: PASS_WITH_WARNINGS' "$report_dir/local-release-warning-selftest.md"
grep -q '| authenticated release-binary smoke | WARN |' "$report_dir/local-release-warning-selftest.md"
grep -q '| backup and restore drill | WARN |' "$report_dir/local-release-warning-selftest.md"
grep -q 'Warnings: 4' "$report_dir/local-release-warning-selftest.md"

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  TNG_RELEASE_EVIDENCE_SELFTEST=1 \
  "$ROOT/scripts/release_evidence_suite.sh" "$report_dir/release-evidence-suite-env-selftest.md" >/dev/null
grep -q "report_dir=$report_dir" "$report_dir/release-evidence-suite-env-selftest.md"
grep -q "benchmark_dir=$benchmark_dir" "$report_dir/release-evidence-suite-env-selftest.md"
grep -q 'Overall status: PASS' "$report_dir/release-evidence-suite-env-selftest.md"

REPORT_DIR="$report_dir" TNG_UNIVERSAL_COMPAT_SELFTEST=1 \
  "$ROOT/scripts/universal_compatibility_certification.sh" "$report_dir/universal-compat-env-selftest.md" >/dev/null
grep -q "Report directory: $report_dir" "$report_dir/universal-compat-env-selftest.md"
grep -q "$report_dir/api-facades-universal-" "$report_dir/universal-compat-env-selftest.md"
grep -q "$report_dir/migration-corpus-universal-" "$report_dir/universal-compat-env-selftest.md"
grep -q "report_dir=$report_dir" "$report_dir/universal-compat-env-selftest.md"
if grep -q "$ROOT/certification/reports" "$report_dir/universal-compat-env-selftest.md"; then
  echo "universal compatibility selftest leaked default report directory" >&2
  exit 1
fi

write_report "$report_dir/universal-compat-selftest.md" PASS_WITH_SKIPS
write_report "$report_dir/universal-live-selftest.md" PASS_WITH_SKIPS
REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/certification_burndown.sh" "$report_dir/certification-burndown-skips.md" >/dev/null
grep -q 'UNIVERSAL_COMPAT_LIVE=1' "$report_dir/certification-burndown-skips.md"
grep -q 'Latest universal-live report `universal-live-selftest.md` may already include a passing local Docker interop leg' "$report_dir/certification-burndown-skips.md"

if TNG_EXTERNAL_PREFLIGHT_STRICT=1 \
  TNG_MIGRATION_CORPUS_DIR="$tmpdir/missing-corpus" \
  "$ROOT/scripts/external_evidence_preflight.sh" "$report_dir/external-preflight-strict.md" >/dev/null 2>&1; then
  echo "strict external preflight accepted missing external evidence" >&2
  exit 1
fi
grep -q 'Overall status: FAIL' "$report_dir/external-preflight-strict.md"
grep -q 'Warnings promoted to failures by TNG_EXTERNAL_PREFLIGHT_STRICT=1' "$report_dir/external-preflight-strict.md"

placeholder_corpus="$tmpdir/placeholder-corpus"
for family in qbittorrent transmission deluge utorrent biglybt tixati rtorrent generic; do
  mkdir -p "$placeholder_corpus/$family"
  printf 'placeholder\n' >"$placeholder_corpus/$family/README.md"
done
if TNG_EXTERNAL_PREFLIGHT_STRICT=1 \
  TNG_MIGRATION_CORPUS_DIR="$placeholder_corpus" \
  "$ROOT/scripts/external_evidence_preflight.sh" "$report_dir/external-preflight-placeholders.md" >/dev/null 2>&1; then
  echo "strict external preflight accepted placeholder corpus files" >&2
  exit 1
fi
grep -q 'missing evidence files' "$report_dir/external-preflight-placeholders.md"

echo "certification policy self-test: PASS"
