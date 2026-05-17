#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

report_dir="$tmpdir/reports"
benchmark_dir="$tmpdir/benchmarks"
mkdir -p "$report_dir" "$benchmark_dir"

cat >"$report_dir/storage-hardware-selftest.md" <<'REPORT'
# TorrentNG Storage Hardware Matrix

- Generated: 2026-05-17T00:00:00Z
- Host: selftest
- Commit: selftest
- Target: /tmp/storage-selftest

Overall status: PASS
REPORT

cat >"$report_dir/storage-uring-graduation-selftest.md" <<'REPORT'
# TorrentNG io_uring Graduation Report

- Generated: 2026-05-17T00:00:00Z
- Host: selftest
- Commit: selftest
- Target: /tmp/storage-selftest

## pread

- Result: PASS
- Selected: pread
- Fixed-buffer strategy: disabled

## uring

- Result: PASS
- Selected: uring
- Fixed-buffer strategy: frame_pool_slots

## Graduation Gates

| Gate | Result |
| --- | --- |
| uring selected | PASS |
| fixed-buffer strategy | INFO: frame_pool_slots |

Overall status: PASS
REPORT

cat >"$report_dir/storage-move-import-selftest.md" <<'REPORT'
# TorrentNG Storage Move/Import Certification

- Generated: 2026-05-17T00:00:00Z
- Host: selftest
- Commit: selftest
- Target: /tmp/storage-selftest

tng_storage_move_import root=/tmp/storage-selftest files=1 mib_per_file=1 bytes=1048576 moved=1 imported=1 deleted=1 root_confined=1

Overall status: PASS
REPORT

TNG_STORAGE_REPORT_DIR="$report_dir" \
  TNG_STORAGE_REPORT_INDEX="$report_dir/storage-certification-index.md" \
  "$ROOT/scripts/storage_certification_index.sh" >/dev/null

index="$report_dir/storage-certification-index.md"

require_row() {
  local kind="$1"
  local result="$2"
  awk -F'|' -v kind="$kind" -v result="$result" '
    NR <= 2 { next }
    {
      for (i = 1; i <= NF; i++) {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", $i)
      }
      if ($3 == kind && $(NF - 1) == result) found = 1
    }
    END { exit found ? 0 : 1 }
  ' "$index"
}

require_row 'hardware matrix' PASS
require_row 'io_uring capability/graduation' PASS
require_row 'move/import' PASS
grep -q 'storage-uring-graduation-selftest.md' "$index"
grep -q '| uring | frame_pool_slots |' "$index"
grep -q '| yes | PASS |' "$index"

write_passing_report() {
  local path="$1"
  {
    echo "# selftest"
    echo
    echo "Overall status: PASS"
  } >"$path"
}

for pattern in \
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
  certification-burndown-selftest.md \
  release-readiness-selftest.md \
  certification-bundle-selftest.md \
  release-evidence-suite-selftest.md \
  pre-engine-release-selftest.md \
  pre-engine-suite-selftest.md; do
  write_passing_report "$report_dir/$pattern"
done
write_passing_report "$benchmark_dir/report-selftest.md"

cat >"$report_dir/migration-corpus-selftest.md" <<'REPORT'
# selftest

Overall status: PASS_WITH_GAPS
REPORT

cat >"$report_dir/memory-roadmap-certification-selftest.md" <<'REPORT'
# TorrentNG Memory Roadmap Certification

| Roadmap item | Status | Evidence |
| --- | --- | --- |
| storage selftest | PASS | generated |

Overall status: PASS
REPORT

cat >"$report_dir/storage-release-certification-selftest.md" <<'REPORT'
# TorrentNG Storage Release Certification

Overall status: PASS
REPORT

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/post_soak_release_gate.sh" "$report_dir/post-soak-release-selftest-warn.md" >/dev/null
grep -q 'certification status rollup | WARN' "$report_dir/post-soak-release-selftest-warn.md"

cat >"$report_dir/local-release-selftest.md" <<'REPORT'
# selftest

Overall status: PASS_WITH_WARNINGS
REPORT

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/post_soak_release_gate.sh" "$report_dir/post-soak-release-selftest-local-warn.md" >/dev/null
grep -q 'certification status rollup | WARN' "$report_dir/post-soak-release-selftest-local-warn.md"

cat >"$report_dir/migration-corpus-selftest.md" <<'REPORT'
# selftest

Overall status: PASS
REPORT
write_passing_report "$report_dir/local-release-selftest.md"

REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/post_soak_release_gate.sh" "$report_dir/post-soak-release-selftest-pass.md" >/dev/null
grep -q 'storage certification index | PASS' "$report_dir/post-soak-release-selftest-pass.md"
BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$report_dir" >"$tmpdir/status-pass.md"
grep -q '| Storage indexed hardware evidence | PASS | storage-certification-index.md |' "$tmpdir/status-pass.md"
grep -q '| Storage indexed io_uring evidence | PASS | storage-certification-index.md |' "$tmpdir/status-pass.md"
grep -q '| Storage indexed move/import evidence | PASS | storage-certification-index.md |' "$tmpdir/status-pass.md"

cat >"$report_dir/storage-move-import-selftest.md" <<'REPORT'
# TorrentNG Storage Move/Import Certification

- Generated: 2026-05-17T00:00:00Z
- Host: selftest
- Commit: selftest
- Target: /tmp/storage-selftest

Overall status: FAIL
REPORT

TNG_STORAGE_REPORT_DIR="$report_dir" \
  TNG_STORAGE_REPORT_INDEX="$report_dir/storage-certification-index.md" \
  "$ROOT/scripts/storage_certification_index.sh" >/dev/null

if require_row 'move/import' PASS; then
  echo "move/import FAIL report was indexed as PASS" >&2
  exit 1
fi

require_row 'move/import' FAIL
BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$report_dir" >"$tmpdir/status-fail.md"
grep -q '| Storage indexed move/import evidence | FAIL | storage-certification-index.md |' "$tmpdir/status-fail.md"

if REPORT_DIR="$report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/post_soak_release_gate.sh" "$report_dir/post-soak-release-selftest-fail.md" >/dev/null 2>&1; then
  echo "post-soak gate accepted a failing move/import storage evidence category" >&2
  exit 1
fi
grep -q 'storage certification index | FAIL' "$report_dir/post-soak-release-selftest-fail.md"
REPORT_DIR="$report_dir" "$ROOT/scripts/storage_release_certification.sh" --help >/dev/null 2>&1

if TNG_STORAGE_REPORT_DIR="$report_dir" \
  TNG_STORAGE_RELEASE_SELFTEST=1 \
  TNG_STORAGE_SKIP_URING=1 \
  "$ROOT/scripts/storage_release_certification.sh" "$tmpdir/storage-root" >/dev/null 2>&1; then
  echo "storage release certification accepted skipped io_uring without dry-run allowance" >&2
  exit 1
fi

TNG_STORAGE_REPORT_DIR="$report_dir" \
  TNG_STORAGE_RELEASE_SELFTEST=1 \
  TNG_STORAGE_SKIP_URING=1 \
  TNG_STORAGE_SKIP_MOVE_IMPORT=1 \
  TNG_STORAGE_ALLOW_RELEASE_SKIP=1 \
  "$ROOT/scripts/storage_release_certification.sh" "$tmpdir/storage-root" >/dev/null

mkdir -p "$tmpdir/migration-corpus/qbittorrent"
printf 'fixture' >"$tmpdir/migration-corpus/qbittorrent/sample.fastresume"
TNG_MIGRATION_CORPUS_DIR="$tmpdir/migration-corpus" \
  "$ROOT/scripts/migration_corpus_certification.sh" "$report_dir/migration-corpus-inventory-selftest.md" >/dev/null
grep -q '| qbittorrent | PASS | 1 |' "$report_dir/migration-corpus-inventory-selftest.md"
grep -q 'sample.fastresume' "$report_dir/migration-corpus-inventory-selftest.md"
grep -q 'Overall status: PASS_WITH_GAPS' "$report_dir/migration-corpus-inventory-selftest.md"
"$ROOT/scripts/certification_bundle_selftest.sh" >/dev/null
"$ROOT/scripts/certification_policy_selftest.sh" >/dev/null

echo "storage certification self-test: PASS"
