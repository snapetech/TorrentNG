#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/backend-burndown-scale-release-$(date -u +%Y%m%dT%H%M%SZ).md}"
mkdir -p "$(dirname "$OUT")"

status="PASS"
started_at="$(date +%s)"

{
  echo "# TorrentNG Release-Optimized Backend Scale Evidence"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Kernel: $(uname -srmo)"
  echo "- Rust: $(rustc --version 2>/dev/null || echo unavailable)"
  echo "- Cargo: $(cargo --version 2>/dev/null || echo unavailable)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  if git -C "$ROOT" diff --quiet --ignore-submodules -- && git -C "$ROOT" diff --cached --quiet --ignore-submodules --; then
    echo "- Worktree: clean"
  else
    echo "- Worktree: dirty"
  fi
  echo
  echo "This is the release-optimized synthetic scale suite. It exercises the"
  echo "API projection and storage seams in optimized test binaries; it is not"
  echo "a claim that the production daemon has been certified at 100k torrents."
  echo
  echo "## Command"
  echo
  echo '```text'
  echo 'cargo test -p rt-metrics --release --test scale --locked -- --nocapture'
  echo '```'
  echo
  echo "## Output"
  echo
  echo '```text'
} >"$OUT"

if (cd "$ROOT" && cargo test -p rt-metrics --release --test scale --locked -- --nocapture) >>"$OUT" 2>&1; then
  echo '```' >>"$OUT"
else
  echo '```' >>"$OUT"
  status="FAIL"
fi

{
  echo
  echo "- Duration seconds: $(( $(date +%s) - started_at ))"
  echo
  echo "Overall status: $status"
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
