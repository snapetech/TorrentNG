#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CONFIG="${1:-$ROOT/deploy/docker/sidecar.config.toml}"
OUT="${2:-$ROOT/certification/reports/security-review-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status="PASS"

emit() {
  local check="$1"
  local result="$2"
  local detail="$3"
  printf '| %s | %s | %s |\n' "$check" "$result" "$detail" >> "$OUT"
  if [[ "$result" == "FAIL" ]]; then
    status="FAIL"
  fi
}

value_for() {
  local key="$1"
  sed -n "s/^[[:space:]]*$key[[:space:]]*=[[:space:]]*//p" "$CONFIG" | tail -1
}

{
  echo "# TorrentNG Security Review"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Config: $CONFIG"
  echo
  echo "## Automated Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

if [[ ! -f "$CONFIG" ]]; then
  emit "config exists" "FAIL" "missing $CONFIG"
  echo "$OUT"
  exit 1
fi

allow_scripts="$(value_for allow_scripts | tr -d ' "')"
allowed_dirs="$(value_for allowed_script_dirs)"
tokens="$(value_for api_tokens)"
configured_secret="$(value_for secret_key)"
secret="$(printf '%s' "$configured_secret" | tr -d ' "')"
trust_proxy="$(value_for trust_proxy_header | tr -d ' "')"

if [[ -n "${TNG_API_TOKENS:-}" ]]; then
  tokens="$TNG_API_TOKENS"
fi

if [[ -n "${TNG_SECRET_KEY:-}" ]]; then
  secret="$TNG_SECRET_KEY"
fi

if [[ "$allow_scripts" == "false" || -z "$allow_scripts" ]]; then
  emit "script workflows default" "PASS" "disabled"
else
  if [[ "$allowed_dirs" == "[]" || -z "$allowed_dirs" ]]; then
    emit "script allowlist" "FAIL" "scripts enabled with empty allowed_script_dirs"
  else
    emit "script allowlist" "PASS" "allowed_script_dirs=$allowed_dirs"
  fi
fi

if [[ "$tokens" == *"[]"* || -z "$tokens" ]]; then
  emit "api tokens" "WARN" "no tokens configured; acceptable only behind other auth"
elif [[ "$tokens" == *"change-me"* || "$tokens" == *"cert-token"* ]]; then
  emit "api tokens" "FAIL" "example token present"
else
  emit "api tokens" "PASS" "non-empty and not a known example"
fi

if [[ -z "$configured_secret" ]]; then
  emit "session secret" "PASS" "not applicable for native API-token-only config"
elif [[ -z "$secret" || "$secret" == "change-me" || "$secret" == "certification-only-change-me" ]]; then
  emit "session secret" "FAIL" "example or empty secret_key"
else
  emit "session secret" "PASS" "non-example secret_key configured"
fi

if [[ "$trust_proxy" == "true" ]]; then
  emit "proxy header trust" "WARN" "requires trusted reverse proxy that strips spoofed inbound headers"
else
  emit "proxy header trust" "PASS" "disabled"
fi

{
  echo
  echo "## Manual Checks"
  echo
  echo "- Confirm script directories are owned by the service owner or root and are not world-writable."
  echo "- Confirm metrics and health endpoints are exposed only to trusted networks."
  echo "- Confirm Docker/systemd deployments do not mount workflow script directories writable from untrusted paths."
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" != "FAIL" ]]
