#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/security-scan-$(date -u +%Y%m%dT%H%M%SZ).md}"
IMAGE="${RTNG_SCAN_IMAGE:-rtorrentng:certification}"

mkdir -p "$(dirname "$OUT")"

status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >> "$OUT"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

{
  echo "# rtorrentNG Security Scan"
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
    npm audit --audit-level=high --omit=dev --json > /tmp/rtng-npm-audit.json
  ) >/tmp/rtng-npm-audit.log 2>&1 && npm_result=PASS || npm_result=FAIL
  high="$(jq -r '.metadata.vulnerabilities.high // 0' /tmp/rtng-npm-audit.json 2>/dev/null || echo unknown)"
  critical="$(jq -r '.metadata.vulnerabilities.critical // 0' /tmp/rtng-npm-audit.json 2>/dev/null || echo unknown)"
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
    cargo tree --locked >/tmp/rtng-cargo-tree.txt
  ) >/tmp/rtng-cargo-tree.log 2>&1 && mark "cargo dependency tree" "PASS" "resolved with lockfile" || mark "cargo dependency tree" "FAIL" "cargo tree failed"
else
  mark "cargo dependency tree" "BLOCKED" "cargo not installed"
fi

if command -v docker >/dev/null 2>&1; then
  if docker image inspect "$IMAGE" >/dev/null 2>&1; then
    mark "container image exists" "PASS" "$IMAGE"
  else
    mark "container image exists" "FAIL" "$IMAGE not found"
  fi

  if command -v trivy >/dev/null 2>&1; then
    trivy image --quiet --severity HIGH,CRITICAL --exit-code 1 "$IMAGE" >/tmp/rtng-trivy.txt 2>&1 && \
      mark "trivy image scan" "PASS" "no HIGH/CRITICAL findings" || \
      mark "trivy image scan" "FAIL" "HIGH/CRITICAL findings; see /tmp/rtng-trivy.txt"
  else
    if docker run --rm -v /var/run/docker.sock:/var/run/docker.sock aquasec/trivy:latest image --quiet --severity HIGH,CRITICAL --exit-code 1 "$IMAGE" >/tmp/rtng-trivy.txt 2>&1; then
      mark "trivy image scan" "PASS" "dockerized trivy found no HIGH/CRITICAL findings"
    else
      code=$?
      if grep -qi 'vulnerability' /tmp/rtng-trivy.txt 2>/dev/null; then
        mark "trivy image scan" "FAIL" "dockerized trivy found HIGH/CRITICAL findings; see /tmp/rtng-trivy.txt"
      else
        mark "trivy image scan" "BLOCKED" "dockerized trivy failed with exit $code; see /tmp/rtng-trivy.txt"
      fi
    fi
  fi
else
  mark "container image scan" "BLOCKED" "docker not installed"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" != "FAIL" ]]
