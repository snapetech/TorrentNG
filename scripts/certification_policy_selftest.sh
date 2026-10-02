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

# Report selection must preserve paths containing spaces, including the
# exclusion path used for readiness-only universal-live notes.
spaced_report_dir="$tmpdir/reports with spaces"
mkdir -p "$spaced_report_dir"
write_report "$spaced_report_dir/live-cert-spaced.md" PASS
write_report "$spaced_report_dir/universal-live-good.md" PASS
write_report "$spaced_report_dir/universal-live-soak-ready.md" PASS
BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$spaced_report_dir" >"$tmpdir/status-spaced-path.md"
grep -q '| Live API/app readiness | PASS | live-cert-spaced.md |' "$tmpdir/status-spaced-path.md"
grep -q '| Universal live compatibility | PASS | universal-live-good.md |' "$tmpdir/status-spaced-path.md"

cat >"$spaced_report_dir/live-cert-noisy.md" <<'REPORT'
# selftest

This note quotes the phrase test result: ok but has no completed status.
REPORT
BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$spaced_report_dir" >"$tmpdir/status-noisy-report.md"
grep -q '| Live API/app readiness | RUNNING/UNKNOWN | live-cert-noisy.md |' "$tmpdir/status-noisy-report.md"

special_report_dir="$tmpdir/reports \"quoted
newline"
mkdir -p "$special_report_dir"
write_report "$special_report_dir/live-cert-special.md" PASS
if REPORT_DIR="$special_report_dir" BENCHMARK_DIR="$benchmark_dir" \
  "$ROOT/scripts/certification_status_json.sh" "$tmpdir/status-json-special.json" >/dev/null; then
  :
fi
jq -e '.report_dir | contains("quoted\nnewline")' "$tmpdir/status-json-special.json" >/dev/null
jq -e 'type == "object" and (.gates | type == "array")' "$tmpdir/status-json-special.json" >/dev/null

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
grep -q '| 24h soak | PASS | soak-final-selftest.md |' "$report_dir/status-migration-selector.md"
TNG_DEFER_24H_SOAK=1 BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$report_dir" >"$report_dir/status-soak-deferred.md"
grep -q '| 24h soak | INFO | deferred by TNG_DEFER_24H_SOAK=1 |' "$report_dir/status-soak-deferred.md"
write_report "$report_dir/migration-corpus-selftest.md" PASS

REPORT_DIR="$report_dir" \
TNG_LOCAL_RELEASE_SELFTEST=1 \
  TNG_LOCAL_RELEASE_SELFTEST_REPORT_STATUS=PASS_WITH_WARNINGS \
  env -u TNG_STORAGE_MATRIX_TARGETS \
  "$ROOT/scripts/local_release_gate.sh" "$report_dir/local-release-warning-selftest.md" >/dev/null
grep -q '| migration exported corpus coverage | WARN |' "$report_dir/local-release-warning-selftest.md"
grep -q 'Overall status: PASS_WITH_WARNINGS' "$report_dir/local-release-warning-selftest.md"
grep -q '| authenticated release-binary smoke | WARN |' "$report_dir/local-release-warning-selftest.md"
grep -q '| backup and restore drill | WARN |' "$report_dir/local-release-warning-selftest.md"
grep -q 'Warnings: 4' "$report_dir/local-release-warning-selftest.md"

if REPORT_DIR="$report_dir" \
  TNG_LOCAL_RELEASE_SELFTEST=1 \
  TNG_LOCAL_RELEASE_SELFTEST_REPORT_STATUS=RUNNING/UNKNOWN \
  env -u TNG_STORAGE_MATRIX_TARGETS \
  "$ROOT/scripts/local_release_gate.sh" "$report_dir/local-release-unknown-selftest.md" >/dev/null 2>&1; then
  echo "local release gate accepted an unknown child report status" >&2
  exit 1
fi
grep -q '| authenticated release-binary smoke | FAIL |' "$report_dir/local-release-unknown-selftest.md"

cat >"$report_dir/soak-warning-source-selftest.md" <<'REPORT'
# selftest

Overall status: PASS_WITH_WARNINGS
REPORT
if SOAK_MIN_SAMPLES=0 \
  "$ROOT/scripts/finalize_soak.sh" \
  "$report_dir/soak-warning-source-selftest.md" \
  "$report_dir/soak-final-warning-source-selftest.md" >/dev/null 2>&1; then
  echo "soak finalizer promoted PASS_WITH_WARNINGS to PASS" >&2
  exit 1
fi
grep -q '| source completion | FAIL |' "$report_dir/soak-final-warning-source-selftest.md"
if grep -q 'torrent floor\|Minimum torrents' "$report_dir/soak-final-warning-source-selftest.md"; then
  echo "soak finalizer retained the removed torrent-count gate" >&2
  exit 1
fi

cat >"$report_dir/soak-count-policy-selftest.md" <<'REPORT'
# TorrentNG Soak Sample Policy Selftest

| UTC | Health | Torrents | RSS MB | sync/maindata HTTP |
|---|---:|---:|---:|---:|
| 2026-09-19T00:00:00Z | 200 | 0 | 0 | 200 |
REPORT
SOAK_MIN_TORRENTS=999999 \
  "$ROOT/scripts/soak_status.sh" \
  "$report_dir/soak-count-policy-selftest.md" \
  "$report_dir/soak-count-policy-result.md" >/dev/null
grep -q 'Overall status: IN_PROGRESS' "$report_dir/soak-count-policy-result.md"
grep -q '| Torrent count telemetry | INFO |' "$report_dir/soak-count-policy-result.md"
if grep -q 'torrent floor\|target>=999999' "$report_dir/soak-count-policy-result.md"; then
  echo "soak status retained the removed torrent-count gate" >&2
  exit 1
fi

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
grep -qF "Latest universal-live report \`universal-live-selftest.md\` may already include a passing local Docker interop leg" "$report_dir/certification-burndown-skips.md"

if TNG_EXTERNAL_PREFLIGHT_STRICT=1 \
  TNG_MIGRATION_CORPUS_DIR="$tmpdir/missing-corpus" \
  "$ROOT/scripts/external_evidence_preflight.sh" "$report_dir/external-preflight-strict.md" >/dev/null 2>&1; then
  echo "strict external preflight accepted missing external evidence" >&2
  exit 1
fi
grep -q 'Overall status: FAIL' "$report_dir/external-preflight-strict.md"
grep -q 'Warnings promoted to failures by TNG_EXTERNAL_PREFLIGHT_STRICT=1' "$report_dir/external-preflight-strict.md"

# VPN forwarding state is external input to a shell-sourced env file. Verify
# provider-controlled filenames/diagnostics stay data and that ports with
# leading zeroes are handled as decimal values.
vpn_state_dir="$tmpdir/vpn-state"
mkdir -p "$vpn_state_dir"
vpn_file="$vpn_state_dir/pf-\$(touch vpn-pwned).env"
printf 'local_port=050000\ntarget_port=050000\npublic_port=060001\npublic_ip=203.0.113.7\nproto=tcp\n' >"$vpn_file"
vpn_env="$tmpdir/vpn-forward.env"
(
  cd "$tmpdir"
  TNG_VPN_STATE_DIR="$vpn_state_dir" \
  TNG_VPN_STATIC_FORWARD_DIR="$tmpdir/no-static-forward" \
  TNG_VPN_PRIVATE_PORT=050000 \
  TNG_VPN_OUT_ENV="$vpn_env" \
    "$ROOT/scripts/vpn/tng_forward_from_vpn_state.sh" write-env >/dev/null
)
[[ ! -e "$tmpdir/vpn-pwned" ]]
bash -c 'set -euo pipefail; source "$1"; [[ "$TNG_INCOMING_PORT" == 050000 && "$TNG_VPN_PUBLIC_PORT" == 060001 ]]' _ "$vpn_env"

placeholder_corpus="$tmpdir/placeholder-corpus"
for family in qbittorrent transmission deluge utorrent biglybt tixati rtorrent generic; do
  mkdir -p "$placeholder_corpus/$family"
  printf 'placeholder\n' >"$placeholder_corpus/$family/README.md"
done
TNG_DEFER_24H_SOAK=1 TNG_MIGRATION_CORPUS_DIR="$placeholder_corpus" \
  "$ROOT/scripts/external_evidence_preflight.sh" "$report_dir/external-preflight-soak-deferred.md" >/dev/null 2>&1 || true
grep -q '| 24h soak | INFO | deferred by TNG_DEFER_24H_SOAK=1' "$report_dir/external-preflight-soak-deferred.md"
if TNG_EXTERNAL_PREFLIGHT_STRICT=1 \
  TNG_MIGRATION_CORPUS_DIR="$placeholder_corpus" \
  "$ROOT/scripts/external_evidence_preflight.sh" "$report_dir/external-preflight-placeholders.md" >/dev/null 2>&1; then
  echo "strict external preflight accepted placeholder corpus files" >&2
  exit 1
fi
grep -q 'missing evidence files' "$report_dir/external-preflight-placeholders.md"

# A newly started soak must supersede an older completed finalization.  This
# prevents a previous PASS from masking a current RUNNING or stale report.
write_report "$report_dir/soak-24h-newer-selftest.md" RUNNING/UNKNOWN
BENCHMARK_DIR="$benchmark_dir" "$ROOT/scripts/certification_status.sh" "$report_dir" >"$report_dir/status-newer-soak.md"
grep -q '| 24h soak | STALE/INCOMPLETE | soak-24h-newer-selftest.md |' "$report_dir/status-newer-soak.md"

write_report "$report_dir/soak-failed-source-selftest.md" FAIL
if SOAK_ALLOW_INCOMPLETE=1 \
  "$ROOT/scripts/finalize_soak.sh" \
  "$report_dir/soak-failed-source-selftest.md" \
  "$report_dir/soak-final-failed-source-selftest.md" >/dev/null 2>&1; then
  echo "soak finalizer waived a failed source report" >&2
  exit 1
fi
grep -q '| source completion | FAIL | source report completed FAIL |' \
  "$report_dir/soak-final-failed-source-selftest.md"

failed_release_soak="$report_dir/soak-failed-release-selftest.md"
write_report "$failed_release_soak" FAIL
if REPORT_DIR="$report_dir" SOAK_REPORT="$failed_release_soak" \
  "$ROOT/scripts/pre_engine_release_report.sh" \
  "$report_dir/pre-engine-release-failed-soak-selftest.md" >/dev/null 2>&1; then
  echo "pre-engine release report waived a failed source report" >&2
  exit 1
fi
grep -q '| source report | FAIL | soak-failed-release-selftest.md status=FAIL |' \
  "$report_dir/pre-engine-release-failed-soak-selftest.md"

optional_pre_engine_dir="$tmpdir/reports optional pre-engine"
cp -a "$report_dir" "$optional_pre_engine_dir"
while IFS= read -r -d '' candidate; do
  if ! grep -q '^Overall status: PASS$' "$candidate"; then
    rm -f "$candidate"
  fi
done < <(find "$optional_pre_engine_dir" -maxdepth 1 -type f -print0)
rm -f \
  "$optional_pre_engine_dir/natpmp-dht-selftest.md" \
  "$optional_pre_engine_dir/proton-natpmp-selftest.md" \
  "$optional_pre_engine_dir/soak-status-selftest.md"
REPORT_DIR="$optional_pre_engine_dir" \
  SOAK_REPORT="$optional_pre_engine_dir/soak-24h-selftest.md" \
  TNG_DEFER_24H_SOAK=1 \
  "$ROOT/scripts/pre_engine_release_report.sh" \
  "$optional_pre_engine_dir/pre-engine-release-optional-missing.md" >/dev/null
grep -q '| NAT-PMP DHT | INFO | optional evidence not present:' \
  "$optional_pre_engine_dir/pre-engine-release-optional-missing.md"
grep -q '| Proton NAT-PMP | INFO | optional evidence not present:' \
  "$optional_pre_engine_dir/pre-engine-release-optional-missing.md"
grep -q '| Soak status | INFO | optional evidence not present:' \
  "$optional_pre_engine_dir/pre-engine-release-optional-missing.md"
grep -q 'Overall status: PASS' \
  "$optional_pre_engine_dir/pre-engine-release-optional-missing.md"

load_report="$report_dir/api-load-invalid-url-selftest.md"
probe_marker="$(python3 -c 'import secrets; print(secrets.token_hex(12))')"
if TNG_BASE_URL="http://${probe_marker}:${probe_marker}@example.invalid/api?leak=${probe_marker}" \
  TNG_API_TOKEN="$probe_marker" \
  python3 "$ROOT/scripts/backend_burndown_api_load.py" "$load_report" >/dev/null 2>&1; then
  echo "API load helper accepted a credential-bearing base URL" >&2
  exit 1
fi
grep -q 'Base URL: <invalid base URL>' "$load_report"
grep -q 'Overall status: NOT_RUN' "$load_report"
if grep -Fq "$probe_marker" "$load_report"; then
  echo "API load evidence retained rejected URL credentials or query data" >&2
  exit 1
fi

echo "certification policy self-test: PASS"
