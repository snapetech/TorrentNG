#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/backend-burndown-fault-matrix-$(date -u +%Y%m%dT%H%M%SZ).md}"
STRICT="${TNG_FAULT_MATRIX_STRICT:-0}"
LIVE="${TNG_FAULT_LIVE:-0}"

mkdir -p "$(dirname "$OUT")"
status="PASS"
warnings=0

slug() {
  printf '%s' "$1" | tr '[:upper:] ' '[:lower:]_' | tr -cd '[:alnum:]_-'
}

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >>"$OUT"
  case "$result" in
    PASS) ;;
    WARN)
      warnings=$((warnings + 1))
      if [[ "$STRICT" == "1" ]]; then
        status="FAIL"
      fi
      ;;
    *) status="FAIL" ;;
  esac
}

run_gate() {
  local name="$1"
  shift
  local log="$REPORT_DIR/backend-burndown-fault-$(slug "$name").log"
  if (cd "$ROOT" && "$@") >"$log" 2>&1; then
    mark "$name" PASS "$(basename "$log")"
  else
    mark "$name" FAIL "$(basename "$log"); see log"
  fi
}

{
  echo "# TorrentNG Backend Fault-Containment Matrix"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Strict mode: $STRICT"
  echo "- Live process faults requested: $LIVE"
  echo
  echo "This gate exercises the bounded DB worker, cancellation fencing,"
  echo "transactional rollback, storage-worker supervision, task liveness,"
  echo "and restart-recovery paths. The optional live phase runs the optimized"
  echo "daemon and is required for a complete runtime fault claim."
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Evidence |"
  echo "|---|---|---|"
} >"$OUT"

run_gate "DB worker failure panic cancellation tests" \
  cargo test -p rt-engine --locked db_worker::tests
run_gate "transaction rollback tests" \
  cargo test -p rt-engine --locked add_torrent_rolls_back
run_gate "storage worker cancellation panic failure tests" \
  cargo test -p rt-engine --locked storage_jobs::tests
run_gate "engine liveness and bounded shutdown tests" \
  cargo test -p rt-engine --locked engine_handle_
run_gate "restart recovery tests" \
  cargo test -p rt-engine --locked recovered_
run_gate "production engine compile" \
  cargo check -p rt-engine --release --locked
run_gate "complete engine regression suite" \
  cargo test -p rt-engine --locked

# Keep this harness dependent only on POSIX-ish tools available on the hosted
# runner. `rg` is convenient locally but is not installed on ubuntu-latest.
if awk '
  /#\[cfg\(test\)\]/ { test_attr = 1; next }
  test_attr && /db:[[:space:]]*Arc<Mutex<Connection>>/ { found = 1; exit }
  test_attr && /^[[:space:]]*[^[:space:]]/ { test_attr = 0 }
  END { exit found ? 0 : 1 }
' "$ROOT/crates/rt-engine/src/engine.rs"; then
  mark "engine test-only direct database fixture" PASS "production Engine has no direct SQLite handle"
else
  mark "engine test-only direct database fixture" FAIL "Engine database ownership marker missing"
fi

if grep -Eq 'db_worker: DbWorker' "$ROOT/crates/rt-engine/src/engine.rs" \
  && grep -Eq 'self\.db_worker\.run' "$ROOT/crates/rt-engine/src/engine.rs" \
  && grep -Eq 'pub\(crate\) fn submit_managed' "$ROOT/crates/rt-engine/src/storage_jobs.rs"; then
  mark "supervised persistence ownership boundary" PASS "engine DB and production storage submissions use supervised workers"
else
  mark "supervised persistence ownership boundary" FAIL "supervised persistence markers missing"
fi

if [[ "$LIVE" == "1" ]]; then
  live_report="${OUT%.md}-live.md"
  if TNG_FAULT_MATRIX_STRICT="$STRICT" \
    "$ROOT/scripts/backend_burndown_native_fault_injection.sh" "$live_report" >>"$OUT" 2>&1; then
    mark "live crash cancellation DB-failure storage-failure matrix" PASS "$(basename "$live_report")"
  else
    mark "live crash cancellation DB-failure storage-failure matrix" FAIL "$(basename "$live_report"); see embedded output"
  fi
else
  mark "live crash cancellation DB-failure storage-failure matrix" WARN "set TNG_FAULT_LIVE=1 with a release binary to run it"
fi

{
  echo
  if [[ "$status" == "PASS" && "$warnings" -gt 0 ]]; then
    echo "Overall status: PASS_WITH_WARNINGS"
    echo "Warnings: $warnings"
  else
    echo "Overall status: $status"
    if [[ "$status" == "FAIL" && "$STRICT" == "1" ]]; then
      echo "Warnings promoted to failures by TNG_FAULT_MATRIX_STRICT=1"
    fi
  fi
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
