#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${CERT_ENV_FILE:-$ROOT/deploy/certification/.env}"
OUT="${1:-$ROOT/certification/reports/autobrr-$(date -u +%Y%m%dT%H%M%SZ).md}"

if [[ -f "$ENV_FILE" ]]; then
  set -a
  # shellcheck disable=SC1090
  source "$ENV_FILE"
  set +a
fi

TNG_API_TOKEN="${TNG_API_TOKEN:-cert-token}"
AUTOBRR_CONTAINER="${AUTOBRR_CONTAINER:-certification-autobrr-1}"
AUTOBRR_HOST_URL="${AUTOBRR_HOST_URL:-http://localhost:${AUTOBRR_HOST_PORT:-17474}}"
AUTOBRR_CERT_USER="${AUTOBRR_CERT_USER:-cert}"
AUTOBRR_CERT_PASSWORD="${AUTOBRR_CERT_PASSWORD:-cert}"
COOKIE_JAR="$(mktemp)"
BODY="$(mktemp)"

mapped="$(docker port "$AUTOBRR_CONTAINER" 7474/tcp 2>/dev/null | sed -n 's/.*:\([0-9][0-9]*\)$/\1/p' | head -1 || true)"
if [[ -n "$mapped" && "$AUTOBRR_HOST_URL" == http://localhost:* ]]; then
  AUTOBRR_HOST_URL="http://localhost:$mapped"
fi

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

cleanup() {
  rm -f "$COOKIE_JAR" "$BODY"
}
trap cleanup EXIT

api() {
  local method="$1"
  local path="$2"
  local payload="${3:-}"
  if [[ -n "$payload" ]]; then
    curl -ksS -b "$COOKIE_JAR" -o "$BODY" -w '%{http_code}' -H 'Content-Type: application/json' -X "$method" -d "$payload" "$AUTOBRR_HOST_URL$path"
  else
    curl -ksS -b "$COOKIE_JAR" -o "$BODY" -w '%{http_code}' -X "$method" "$AUTOBRR_HOST_URL$path"
  fi
}

delete_named() {
  local path="$1"
  local jq_expr="$2"
  curl -fsS -b "$COOKIE_JAR" "$AUTOBRR_HOST_URL$path" \
    | jq -r "$jq_expr" \
    | while read -r id; do
        [[ -n "$id" ]] && curl -ksS -b "$COOKIE_JAR" -o /dev/null -X DELETE "$AUTOBRR_HOST_URL$path/$id"
      done
}

{
  echo "# TorrentNG autobrr Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- autobrr URL: $AUTOBRR_HOST_URL"
  echo "- Scope: login/onboard, qBittorrent downloader test, filter/action create, readback"
  echo
  echo "## Checks"
  echo
  echo "| Check | Result | Detail |"
  echo "|---|---|---|"
} > "$OUT"

code="$(curl -ksS -o "$BODY" -w '%{http_code}' "$AUTOBRR_HOST_URL/api/auth/onboard")"
if [[ "$code" == "204" ]]; then
  code="$(curl -ksS -o "$BODY" -w '%{http_code}' -H 'Content-Type: application/json' -X POST -d "{\"username\":\"$AUTOBRR_CERT_USER\",\"password\":\"$AUTOBRR_CERT_PASSWORD\"}" "$AUTOBRR_HOST_URL/api/auth/onboard")"
  [[ "$code" == "204" ]] && mark "onboard" "PASS" "created cert user" || mark "onboard" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
else
  mark "onboard" "PASS" "already initialized"
fi

code="$(curl -ksS -c "$COOKIE_JAR" -o "$BODY" -w '%{http_code}' -H 'Content-Type: application/json' -X POST -d "{\"username\":\"$AUTOBRR_CERT_USER\",\"password\":\"$AUTOBRR_CERT_PASSWORD\",\"remember_me\":true}" "$AUTOBRR_HOST_URL/api/auth/login")"
if [[ "$code" == "204" ]]; then
  mark "login" "PASS" "session cookie accepted"
else
  mark "login" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  echo >> "$OUT"; echo "Overall status: $status" >> "$OUT"; echo "$OUT"; exit 1
fi

client="$(curl -fsS -b "$COOKIE_JAR" "$AUTOBRR_HOST_URL/api/download_clients" | jq -c '.[] | select(.name=="TorrentNG-qBit")' | head -1)"
if [[ -n "$client" ]]; then
  client_id="$(jq -r '.id' <<<"$client")"
  mark "qBit client exists" "PASS" "id=$client_id"
else
  payload="$(jq -nc --arg token "$TNG_API_TOKEN" '{
    name: "TorrentNG-qBit",
    type: "QBITTORRENT",
    enabled: true,
    host: "torrentng",
    port: 8080,
    tls: false,
    tls_skip_verify: false,
    username: $token,
    password: $token,
    settings: {basic: {}, rules: {enabled: false}}
  }')"
  code="$(api POST /api/download_clients "$payload")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    client_id="$(jq -r '.id' "$BODY")"
    mark "qBit client exists" "PASS" "created id=$client_id"
  else
    mark "qBit client exists" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
    client_id=""
  fi
fi

if [[ -n "${client_id:-}" ]]; then
  test_payload="$(jq -nc --argjson id "$client_id" --arg token "$TNG_API_TOKEN" '{
    id: $id,
    name: "TorrentNG-qBit",
    type: "QBITTORRENT",
    enabled: true,
    host: "torrentng",
    port: 8080,
    tls: false,
    tls_skip_verify: false,
    username: $token,
    password: $token,
    settings: {basic: {}, rules: {enabled: false}}
  }')"
  code="$(api POST /api/download_clients/test "$test_payload")"
  if [[ "$code" == "204" ]]; then
    mark "qBit client test" "PASS" "autobrr reached TorrentNG"
  else
    mark "qBit client test" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  fi
fi

delete_named /api/actions '.[] | select(.name=="TorrentNG qBit Action") | .id'
delete_named /api/filters '.[] | select(.name=="TorrentNG Fixture Filter") | .id'

filter_payload="$(jq -nc '{
  name: "TorrentNG Fixture Filter",
  enabled: true,
  match_releases: "TNG",
  use_regex: false,
  resolutions: [],
  codecs: [],
  sources: [],
  containers: [],
  match_hdr: [],
  except_hdr: [],
  match_other: [],
  except_other: [],
  release_types_match: [],
  release_types_ignore: [],
  formats: [],
  quality: [],
  media: [],
  match_language: [],
  except_language: [],
  origins: [],
  except_origins: [],
  announce_types: []
}')"
code="$(api POST /api/filters "$filter_payload")"
if [[ "$code" == "200" || "$code" == "201" ]]; then
  filter_id="$(jq -r '.id' "$BODY")"
  mark "filter create" "PASS" "id=$filter_id"
else
  mark "filter create" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  filter_id=""
fi

if [[ -n "${client_id:-}" && -n "${filter_id:-}" ]]; then
  action_payload="$(jq -nc --argjson client_id "$client_id" --argjson filter_id "$filter_id" '{
    name: "TorrentNG qBit Action",
    type: "QBITTORRENT",
    enabled: true,
    client_id: $client_id,
    filter_id: $filter_id,
    category: "autobrr",
    save_path: "/data/autobrr",
    paused: false,
    ignore_rules: false
  }')"
  code="$(api POST /api/actions "$action_payload")"
  if [[ "$code" == "200" || "$code" == "201" ]]; then
    action_id="$(jq -r '.id' "$BODY")"
    mark "qBit action create" "PASS" "id=$action_id filter=$filter_id client=$client_id"
  else
    mark "qBit action create" "FAIL" "HTTP $code $(tr '\n' ' ' <"$BODY")"
  fi
fi

actions="$(curl -fsS -b "$COOKIE_JAR" "$AUTOBRR_HOST_URL/api/actions" | jq '[.[] | select(.name=="TorrentNG qBit Action" and .type=="QBITTORRENT" and .enabled==true)] | length')"
filters="$(curl -fsS -b "$COOKIE_JAR" "$AUTOBRR_HOST_URL/api/filters" | jq '[.[] | select(.name=="TorrentNG Fixture Filter" and .enabled==true)] | length')"
if [[ "$actions" -gt 0 && "$filters" -gt 0 ]]; then
  mark "readback" "PASS" "filters=$filters actions=$actions"
else
  mark "readback" "FAIL" "filters=$filters actions=$actions"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
