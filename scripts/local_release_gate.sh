#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/local-release-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
started_at="$(date +%s)"

run_gate() {
  local name="$1"
  shift
  local start end elapsed result
  start="$(date +%s)"
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
    echo '```' >>"$OUT"
  else
    result="FAIL"
    echo '```' >>"$OUT"
    status="FAIL"
  fi
  end="$(date +%s)"
  elapsed="$((end - start))s"
  printf '| %s | %s | %s |\n' "$name" "$result" "$elapsed" >>"$OUT.table"
}

skip_gate() {
  local name="$1"
  local reason="$2"
  {
    echo
    echo "## $name"
    echo
    echo '```text'
    echo "SKIP: $reason"
    echo '```'
  } >>"$OUT"
  printf '| %s | SKIP | 0s |\n' "$name" >>"$OUT.table"
}

{
  echo "# TorrentNG Local Release Gate"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Kernel: $(uname -srmo)"
  echo "- Rust: $(rustc --version 2>/dev/null || echo unavailable)"
  echo "- Cargo: $(cargo --version 2>/dev/null || echo unavailable)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Branch: $(git -C "$ROOT" rev-parse --abbrev-ref HEAD 2>/dev/null || echo unavailable)"
  if git -C "$ROOT" diff --quiet --ignore-submodules -- && git -C "$ROOT" diff --cached --quiet --ignore-submodules --; then
    echo "- Worktree: clean"
  else
    echo "- Worktree: dirty"
  fi
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result | Duration |"
  echo "|---|---|---|"
} >"$OUT"
: >"$OUT.table"

{
  echo
  echo "## Git Status"
  echo
  echo '```text'
  git -C "$ROOT" status --short
  echo '```'
} >>"$OUT"

run_gate "format" cargo fmt --check
run_gate "workspace tests" cargo test --workspace
run_gate "Storage NG feature matrix" "$ROOT/scripts/storage_ng_feature_matrix.sh"
run_gate "API facade certification" "$ROOT/scripts/api_facade_certification.sh" "$REPORT_DIR/api-facades-local-release-$(date -u +%Y%m%dT%H%M%SZ).md"

run_gate "native config security review" bash -c '
  set -euo pipefail
  TNG_API_TOKENS="${TNG_API_TOKENS:-local-release-native-token}" \
    "$1/scripts/security_review.sh" "$1/deploy/native/config.toml" "$2/security-review-native-local-$(date -u +%Y%m%dT%H%M%SZ).md"
' _ "$ROOT" "$REPORT_DIR"

run_gate "sidecar config security review" bash -c '
  set -euo pipefail
  TNG_API_TOKENS="${TNG_API_TOKENS:-local-release-sidecar-token}" \
  TNG_SECRET_KEY="${TNG_SECRET_KEY:-local-release-sidecar-secret-00000000000000000000}" \
    "$1/scripts/security_review.sh" "$1/deploy/docker/sidecar.config.toml" "$2/security-review-sidecar-local-$(date -u +%Y%m%dT%H%M%SZ).md"
' _ "$ROOT" "$REPORT_DIR"

if [[ -n "${TNG_STORAGE_MATRIX_TARGETS:-}" ]]; then
  # shellcheck disable=SC2086 # Intentional whitespace-separated target list.
  run_gate "storage release certification" "$ROOT/scripts/storage_release_certification.sh" $TNG_STORAGE_MATRIX_TARGETS
else
  skip_gate "storage release certification" "set TNG_STORAGE_MATRIX_TARGETS='/mnt/nvme /mnt/hdd' to run real-device probes"
fi

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
  echo "Total duration: $(($(date +%s) - started_at))s"
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
