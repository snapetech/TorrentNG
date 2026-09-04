#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/security-scan-$(date -u +%Y%m%dT%H%M%SZ).md}"
IMAGE="${TNG_SCAN_IMAGE:-torrentng:certification}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
blocked=0

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  elif [[ "$result" == "BLOCKED" ]]; then
    blocked=1
  fi
}

{
  echo "# TorrentNG Security Scan"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Image: $IMAGE"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if command -v npm >/dev/null 2>&1; then
  (
    cd "$ROOT/webui"
    npm audit --audit-level=high --omit=dev --json > /tmp/tng-npm-audit.json
  ) >/tmp/tng-npm-audit.log 2>&1 && npm_result=PASS || npm_result=FAIL
  high="$(jq -r '.metadata.vulnerabilities.high // 0' /tmp/tng-npm-audit.json 2>/dev/null || echo unknown)"
  critical="$(jq -r '.metadata.vulnerabilities.critical // 0' /tmp/tng-npm-audit.json 2>/dev/null || echo unknown)"
  if [[ "$npm_result" == "PASS" ]]; then
    mark "npm production audit" "PASS" "high=$high critical=$critical"
  else
    mark "npm production audit" "FAIL" "high=$high critical=$critical; see npm audit output"
  fi
else
  mark "npm production audit" "BLOCKED" "npm not installed"
fi

if command -v cargo >/dev/null 2>&1; then
  (
    cd "$ROOT/sidecar"
    cargo tree --locked >/tmp/tng-cargo-tree.txt
  ) >/tmp/tng-cargo-tree.log 2>&1 && mark "cargo dependency tree" "PASS" "resolved with lockfile" || mark "cargo dependency tree" "FAIL" "cargo tree failed"
else
  mark "cargo dependency tree" "BLOCKED" "cargo not installed"
fi

if command -v cargo-audit >/dev/null 2>&1; then
  if (cd "$ROOT" && cargo audit) >/tmp/tng-cargo-audit.log 2>&1; then
    mark "cargo advisory audit" "PASS" "no actionable RustSec advisories"
  else
    mark "cargo advisory audit" "FAIL" "RustSec audit failed; see /tmp/tng-cargo-audit.log"
  fi
else
  mark "cargo advisory audit" "BLOCKED" "cargo-audit not installed; CI security-audit provides the release gate"
fi

if command -v docker >/dev/null 2>&1; then
  if docker image inspect "$IMAGE" >/dev/null 2>&1; then
    mark "container image exists" "PASS" "$IMAGE"
  else
    mark "container image exists" "FAIL" "$IMAGE not found"
  fi

  if command -v trivy >/dev/null 2>&1; then
    trivy image --quiet --severity HIGH,CRITICAL --exit-code 1 "$IMAGE" >/tmp/tng-trivy.txt 2>&1 && \
      mark "trivy image scan" "PASS" "no HIGH/CRITICAL findings" || \
      mark "trivy image scan" "FAIL" "HIGH/CRITICAL findings; see /tmp/tng-trivy.txt"
  else
    if docker run --rm -v /var/run/docker.sock:/var/run/docker.sock aquasec/trivy:latest image --quiet --severity HIGH,CRITICAL --exit-code 1 "$IMAGE" >/tmp/tng-trivy.txt 2>&1; then
      mark "trivy image scan" "PASS" "dockerized trivy found no HIGH/CRITICAL findings"
    else
      code=$?
      if grep -qi 'vulnerability' /tmp/tng-trivy.txt 2>/dev/null; then
        mark "trivy image scan" "FAIL" "dockerized trivy found HIGH/CRITICAL findings; see /tmp/tng-trivy.txt"
      else
        mark "trivy image scan" "BLOCKED" "dockerized trivy failed with exit $code; see /tmp/tng-trivy.txt"
      fi
    fi
  fi
else
  mark "container image scan" "BLOCKED" "docker not installed"
fi

{
  if [[ "$blocked" == "1" ]]; then
    if [[ "${TNG_SECURITY_SCAN_ALLOW_BLOCKED:-0}" == "1" ]]; then
      status="PASS_WITH_WARNINGS"
    elif [[ "$status" != "FAIL" ]]; then
      status="FAIL"
    fi
  fi
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" || "$status" == "PASS_WITH_WARNINGS" && "${TNG_SECURITY_SCAN_ALLOW_BLOCKED:-0}" == "1" ]]
