#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
COMPOSE_FILE="$ROOT/deploy/interop/compose.yml"
PUBLIC_TOML="$ROOT/deploy/interop/public-torrents.toml"
WORKDIR="${INTEROP_WORKDIR:-$ROOT/certification/interop}"
REPORT_DIR="$ROOT/certification/reports"
STAMP="$(date -u +%Y%m%dT%H%M%SZ)"
REPORT="${INTEROP_REPORT:-$REPORT_DIR/interop-matrix-$STAMP.md}"

MODE="all"
TIMEOUT_LOCAL="${INTEROP_LOCAL_TIMEOUT_SECS:-900}"
TIMEOUT_PUBLIC="${INTEROP_PUBLIC_TIMEOUT_SECS:-7200}"
PUBLIC_MAX_PARALLEL="${INTEROP_PUBLIC_MAX_PARALLEL:-3}"
PUBLIC_MIN_RUST_PEERS="${INTEROP_PUBLIC_MIN_RUST_PEERS:-2}"
KEEP_STACK="${INTEROP_KEEP_STACK:-0}"
KEEP_PUBLIC_DATA="${INTEROP_KEEP_PUBLIC_DATA:-0}"
RUST_TOKEN="${INTEROP_RUST_TOKEN:-interop-token}"
CURL_MAX_TIME="${INTEROP_CURL_MAX_TIME:-10}"
EXTENDED_LOCAL="${INTEROP_EXTENDED_LOCAL:-1}"

CLIENTS=(torrentngd qbittorrent transmission deluge rtorrent)
LOCAL_CASES=(
  "rust-pulls-from-qbit|qbittorrent|torrentngd|single-16m"
  "rust-pulls-from-transmission|transmission|torrentngd|single-16m"
  "rust-pulls-from-deluge|deluge|torrentngd|single-16m"
  "rust-pulls-from-rtorrent|rtorrent|torrentngd|single-16m"
  "qbit-pulls-from-rust|torrentngd|qbittorrent|single-16m"
  "transmission-pulls-from-rust|torrentngd|transmission|single-16m"
  "deluge-pulls-from-rust|torrentngd|deluge|single-16m"
  "rtorrent-pulls-from-rust|torrentngd|rtorrent|single-16m"
  "mesh-swarm|all|all|multi-128m"
  "churn|rotating|rotating|churn"
)

usage() {
  cat <<'USAGE'
Usage: scripts/interop_matrix.sh [--local|--public|--all] [--report PATH]

Environment:
  INTEROP_LOCAL_TIMEOUT_SECS=900
  INTEROP_PUBLIC_TIMEOUT_SECS=7200
  INTEROP_PUBLIC_MAX_PARALLEL=3
  INTEROP_PUBLIC_MIN_RUST_PEERS=2
  INTEROP_INCLUDE_LIBREOFFICE=1
  INTEROP_PUBLIC_ONLY=debian
  INTEROP_EXTENDED_ONLY=1
  INTEROP_SKIP_BUILD=1
  INTEROP_KEEP_STACK=1
  INTEROP_KEEP_PUBLIC_DATA=0
  INTEROP_CURL_MAX_TIME=10
  INTEROP_EXTENDED_LOCAL=1
  INTEROP_PROTOCOL_LOCAL=1
  INTEROP_PROTOCOL_ONLY=rust-magnet-with-tracker
  INTEROP_WORKDIR=certification/interop
USAGE
}

while [[ $# -gt 0 ]]; do
  case "$1" in
    --local) MODE="local" ;;
    --public) MODE="public" ;;
    --all) MODE="all" ;;
    --report) shift; REPORT="${1:?missing path for --report}" ;;
    -h|--help) usage; exit 0 ;;
    *) REPORT="$1" ;;
  esac
  shift
done

compose() {
  INTEROP_WORKDIR="$WORKDIR" docker compose -f "$COMPOSE_FILE" "$@"
}

log() {
  printf '[interop] %s\n' "$*" >&2
}

append_report() {
  printf '%s\n' "$*" >>"$REPORT"
}

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || {
    echo "missing required command: $1" >&2
    exit 127
  }
}

client_url() {
  case "$1" in
    torrentngd) echo "http://127.0.0.1:${INTEROP_RUST_HOST_PORT:-28180}" ;;
    qbittorrent) echo "http://127.0.0.1:${INTEROP_QBIT_HOST_PORT:-28181}" ;;
    transmission) echo "http://127.0.0.1:${INTEROP_TRANSMISSION_HOST_PORT:-28191}" ;;
    deluge) echo "http://127.0.0.1:${INTEROP_DELUGE_HOST_PORT:-28212}" ;;
    *) return 1 ;;
  esac
}

download_dir() {
  case "$1" in
    torrentngd) echo "/downloads/torrentngd" ;;
    qbittorrent) echo "/downloads/qbittorrent" ;;
    transmission) echo "/downloads/transmission" ;;
    deluge) echo "/downloads/deluge" ;;
    rtorrent) echo "/downloads/rtorrent" ;;
    *) return 1 ;;
  esac
}

host_download_dir() {
  echo "$WORKDIR/downloads/$1"
}

prepare_dirs() {
  mkdir -p "$REPORT_DIR" "$WORKDIR"/{artifacts,torrents,fixtures,downloads,logs,watch/rtorrent,config}
  for client in "${CLIENTS[@]}"; do
    mkdir -p "$WORKDIR/downloads/$client"
  done
  prepare_client_configs
  chmod -R a+rwX "$WORKDIR/artifacts" "$WORKDIR/downloads" "$WORKDIR/fixtures" "$WORKDIR/torrents" "$WORKDIR/watch" 2>/dev/null || true
  chmod -R a+rwX "$WORKDIR/config/rtorrent" 2>/dev/null || true
}

prepare_client_configs() {
  mkdir -p "$WORKDIR/config/qbittorrent/qBittorrent" "$WORKDIR/config/transmission" "$WORKDIR/config/deluge" "$WORKDIR/config/rtorrent/session"
  if [[ ! -f "$WORKDIR/config/qbittorrent/qBittorrent/qBittorrent.conf" ]]; then
    cat >"$WORKDIR/config/qbittorrent/qBittorrent/qBittorrent.conf" <<'EOF'
[BitTorrent]
Session\DefaultSavePath=/downloads/qbittorrent
Session\Port=6882

[LegalNotice]
Accepted=true

[Preferences]
WebUI\AuthSubnetWhitelist=0.0.0.0/0,::/0
WebUI\AuthSubnetWhitelistEnabled=true
WebUI\LocalHostAuth=false
WebUI\Port=8080
EOF
  fi
}

reset_workdir() {
  if [[ -d "$WORKDIR" ]]; then
    chmod -R u+rwX "$WORKDIR" 2>/dev/null || true
    rm -rf "$WORKDIR/artifacts" "$WORKDIR/config" "$WORKDIR/downloads" "$WORKDIR/fixtures" "$WORKDIR/logs" "$WORKDIR/torrents" "$WORKDIR/watch" 2>/dev/null || \
      docker run --rm -v "$WORKDIR:/work" alpine:3.20 sh -lc 'rm -rf /work/artifacts /work/config /work/downloads /work/fixtures /work/logs /work/torrents /work/watch'
  fi
}

write_report_header() {
  cat >"$REPORT" <<EOF
# Interop Matrix Report

- Generated: $STAMP
- Mode: $MODE
- Compose file: \`deploy/interop/compose.yml\`
- Workdir: \`$WORKDIR\`
- Public timeout: ${TIMEOUT_PUBLIC}s
- Local timeout: ${TIMEOUT_LOCAL}s

EOF
}

cleanup() {
  local status=$?
  capture_artifacts || true
  if [[ "$KEEP_STACK" != "1" ]]; then
    compose down --remove-orphans >/dev/null 2>&1 || true
  else
    log "keeping interop stack because INTEROP_KEEP_STACK=1"
  fi
  exit "$status"
}

capture_artifacts() {
  mkdir -p "$WORKDIR/logs/$STAMP"
  compose ps >"$WORKDIR/logs/$STAMP/compose-ps.txt" 2>&1 || true
  for service in torrentngd qbittorrent transmission deluge rtorrent opentracker fixture-http; do
    compose logs --no-color --tail=250 "$service" >"$WORKDIR/logs/$STAMP/$service.log" 2>&1 || true
  done
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/health" >"$WORKDIR/logs/$STAMP/rust-health.json" 2>/dev/null || true
  curl --max-time "$CURL_MAX_TIME" -fsS "$(client_url torrentngd)/metrics" >"$WORKDIR/logs/$STAMP/rust-metrics.txt" 2>/dev/null || true
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/v1/torrents" >"$WORKDIR/logs/$STAMP/rust-torrents.json" 2>/dev/null || true
}

wait_http() {
  local name="$1" url="$2" timeout="${3:-180}" start
  start="$(date +%s)"
  until curl --max-time "$CURL_MAX_TIME" -fsS "$url" >/dev/null 2>&1; do
    if (( "$(date +%s)" - start > timeout )); then
      echo "timed out waiting for $name at $url" >&2
      return 1
    fi
    sleep 2
  done
}

wait_http_status() {
  local name="$1" url="$2" pattern="$3" timeout="${4:-180}" start code
  start="$(date +%s)"
  while true; do
    code="$(curl --max-time "$CURL_MAX_TIME" -sS -o /dev/null -w '%{http_code}' "$url" 2>/dev/null || true)"
    if [[ "$code" =~ $pattern ]]; then
      return 0
    fi
    if (( "$(date +%s)" - start > timeout )); then
      echo "timed out waiting for $name at $url; last HTTP status: ${code:-none}" >&2
      return 1
    fi
    sleep 2
  done
}

wait_stack() {
  log "waiting for client APIs and ports"
  wait_http torrentngd "$(client_url torrentngd)/health" 240
  wait_http_status qbittorrent "$(client_url qbittorrent)" '^(200|401|403)$' 240
  wait_http transmission "$(client_url transmission)/transmission/web/" 240
  wait_http deluge "$(client_url deluge)" 240
  local rtorrent_id
  rtorrent_id="$(compose ps -q rtorrent)"
  [[ -n "$rtorrent_id" ]] && [[ "$(docker inspect -f '{{.State.Running}}' "$rtorrent_id")" == "true" ]]
}

docker_tool() {
  docker run --rm -v "$WORKDIR:/work" -w /work alpine:3.20 sh -lc "$*"
}

ensure_mktorrent() {
  if command -v mktorrent >/dev/null 2>&1; then
    echo "host"
  else
    echo "container"
  fi
}

create_fixture_files() {
  log "creating deterministic local fixtures"
  mkdir -p "$WORKDIR/fixtures/single-16m" "$WORKDIR/fixtures/single-64m" "$WORKDIR/fixtures/multi-128m" "$WORKDIR/fixtures/churn"
  if [[ ! -f "$WORKDIR/fixtures/single-16m/payload.bin" ]]; then
    dd if=/dev/zero of="$WORKDIR/fixtures/single-16m/payload.bin" bs=1M count=16 status=none
  fi
  if [[ ! -f "$WORKDIR/fixtures/single-64m/payload.bin" ]]; then
    dd if=/dev/zero of="$WORKDIR/fixtures/single-64m/payload.bin" bs=1M count=64 status=none
  fi
  if [[ ! -f "$WORKDIR/fixtures/multi-128m/part-07.bin" ]]; then
    for i in $(seq -w 0 7); do
      dd if=/dev/zero of="$WORKDIR/fixtures/multi-128m/part-$i.bin" bs=1M count=16 status=none
    done
  fi
  if [[ ! -f "$WORKDIR/fixtures/churn/churn-24.bin" ]]; then
    for i in $(seq -w 0 24); do
      dd if=/dev/zero of="$WORKDIR/fixtures/churn/churn-$i.bin" bs=256K count=1 status=none
    done
  fi
  find "$WORKDIR/fixtures" -type f -print0 | sort -z | xargs -0 sha256sum >"$WORKDIR/artifacts/fixture-sha256sums.txt"
}

case_fixture() {
  local base="$1" case_name="$2" out
  out="$case_name"
  rm -rf "$WORKDIR/fixtures/$out"
  cp -a "$WORKDIR/fixtures/$base" "$WORKDIR/fixtures/$out"
  echo "$out"
}

make_torrent() {
  local fixture="$1" name="$2" mode="${3:-tracker-webseed}" out
  out="$WORKDIR/torrents/$name.torrent"
  [[ -f "$out" ]] && { echo "$out"; return; }
  local tracker="http://opentracker:6969/announce"
  local webseed="http://fixture-http/$fixture"
  local args=()
  case "$mode" in
    tracker-webseed) args=(-a "$tracker" -w "$webseed") ;;
    tracker-only) args=(-a "$tracker") ;;
    multi-tracker-fallback) args=(-a "http://127.0.0.1:9/dead-announce" -a "$tracker") ;;
    udp-tracker-only) args=(-a "udp://opentracker:6969/announce") ;;
    webseed-only) args=(-w "$webseed") ;;
    private-explicit) args=(-p) ;;
    *) echo "unknown torrent mode: $mode" >&2; return 1 ;;
  esac
  if [[ "$(ensure_mktorrent)" == "host" ]]; then
    mktorrent "${args[@]}" -o "$out" "$WORKDIR/fixtures/$fixture" >/dev/null
  else
    local cmd_args=""
    case "$mode" in
      tracker-webseed) cmd_args="-a '$tracker' -w '$webseed'" ;;
      tracker-only) cmd_args="-a '$tracker'" ;;
      multi-tracker-fallback) cmd_args="-a 'http://127.0.0.1:9/dead-announce' -a '$tracker'" ;;
      udp-tracker-only) cmd_args="-a 'udp://opentracker:6969/announce'" ;;
      webseed-only) cmd_args="-w '$webseed'" ;;
      private-explicit) cmd_args="-p" ;;
    esac
    docker_tool "apk add --no-cache mktorrent >/dev/null && mktorrent $cmd_args -o '/work/torrents/$name.torrent' '/work/fixtures/$fixture' >/dev/null" >/dev/null
  fi
  echo "$out"
}

seed_fixture_for_client() {
  local client="$1" fixture="$2" dest
  dest="$(host_download_dir "$client")"
  [[ -n "$dest" && -n "$fixture" ]]
  rm -rf "${dest:?}/$fixture"
  cp -a "$WORKDIR/fixtures/$fixture" "$dest/"
  chmod -R a+rwX "$dest/$fixture" 2>/dev/null || true
}

copy_torrent_to_rtorrent_watch() {
  local torrent="$1"
  cp "$torrent" "$WORKDIR/watch/rtorrent/"
}

qb_login() {
  local pass
  if curl --max-time "$CURL_MAX_TIME" -fsS -H 'Host: localhost:8080' -c "$WORKDIR/artifacts/qbit.cookie" -d 'username=admin&password=adminadmin' "$(client_url qbittorrent)/api/v2/auth/login" >/dev/null; then
    return 0
  fi
  pass="$(compose logs --no-color qbittorrent 2>/dev/null | sed -n 's/.*temporary password is provided for this session: //p' | tail -n1)"
  [[ -n "$pass" ]] || return 1
  curl --max-time "$CURL_MAX_TIME" -fsS -H 'Host: localhost:8080' -c "$WORKDIR/artifacts/qbit.cookie" --data-urlencode 'username=admin' --data-urlencode "password=$pass" "$(client_url qbittorrent)/api/v2/auth/login" >/dev/null
}

add_qb() {
  local torrent="$1" save_path="$2" add_mode="${3:-leecher}" skip=()
  [[ -r "$torrent" ]] || { log "qBittorrent torrent file is not readable: $(printf '%q' "$torrent")"; return 1; }
  [[ "$add_mode" == "seed" ]] && skip=(-F "skip_checking=true")
  qb_login
  curl --max-time "$CURL_MAX_TIME" -fsS -H 'Host: localhost:8080' -b "$WORKDIR/artifacts/qbit.cookie" \
    -F "torrents=@$torrent" \
    -F "savepath=$save_path" \
    -F "paused=false" \
    "${skip[@]}" \
    "$(client_url qbittorrent)/api/v2/torrents/add" >/dev/null
}

qb_force_start() {
  local info_hash="$1"
  qb_login
  curl --max-time "$CURL_MAX_TIME" -fsS -H 'Host: localhost:8080' -b "$WORKDIR/artifacts/qbit.cookie" \
    --data-urlencode "hashes=$info_hash" \
    --data-urlencode "value=true" \
    "$(client_url qbittorrent)/api/v2/torrents/setForceStart" >/dev/null
}

add_rust() {
  local torrent="$1" save_path="$2"
  [[ -r "$torrent" ]] || { log "torrentngd torrent file is not readable: $(printf '%q' "$torrent")"; return 1; }
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    -F "torrents=@$torrent" \
    -F "savepath=$save_path" \
    -F "paused=false" \
    "$(client_url torrentngd)/api/qb/v2/torrents/add" >/dev/null
}

add_rust_url() {
  local url="$1" save_path="$2"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    -F "urls=$url" \
    -F "savepath=$save_path" \
    -F "paused=false" \
    "$(client_url torrentngd)/api/qb/v2/torrents/add" >/dev/null
}

transmission_rpc() {
  local body="$1" url sid
  url="$(client_url transmission)/transmission/rpc"
  sid="$(curl --max-time "$CURL_MAX_TIME" -sS -D - -o /dev/null -H "Content-Type: application/json" -d "$body" "$url" | awk 'tolower($0) ~ /^x-transmission-session-id:/ {print $2}' | tr -d '\r')"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "X-Transmission-Session-Id: $sid" -H "Content-Type: application/json" -d "$body" "$url"
}

add_transmission() {
  local torrent="$1" save_path="$2" metainfo
  [[ -r "$torrent" ]] || { log "Transmission torrent file is not readable: $(printf '%q' "$torrent")"; return 1; }
  metainfo="$(base64 -w0 "$torrent")"
  transmission_rpc "{\"method\":\"torrent-add\",\"arguments\":{\"metainfo\":\"$metainfo\",\"download-dir\":\"$save_path\",\"paused\":false}}" >/dev/null
}

deluge_rpc() {
  local body="$1"
  curl --max-time "$CURL_MAX_TIME" -fsS -c "$WORKDIR/artifacts/deluge.cookie" -b "$WORKDIR/artifacts/deluge.cookie" \
    -H "Content-Type: application/json" -d "$body" "$(client_url deluge)/json"
}

deluge_rpc_file() {
  local body_file="$1"
  curl --max-time "$CURL_MAX_TIME" -fsS -c "$WORKDIR/artifacts/deluge.cookie" -b "$WORKDIR/artifacts/deluge.cookie" \
    -H "Content-Type: application/json" --data-binary "@$body_file" "$(client_url deluge)/json"
}

deluge_rpc_checked() {
  local body="$1" response
  response="$(deluge_rpc "$body")"
  jq -e '.error == null' <<<"$response" >/dev/null
  printf '%s\n' "$response"
}

deluge_rpc_file_checked() {
  local body_file="$1" response
  response="$(deluge_rpc_file "$body_file")"
  jq -e '.error == null' <<<"$response" >/dev/null
  printf '%s\n' "$response"
}

deluge_login() {
  deluge_rpc_checked '{"method":"auth.login","params":["deluge"],"id":1}' >/dev/null
}

deluge_connect() {
  local connected host_id
  deluge_login
  connected="$(deluge_rpc_checked '{"method":"web.connected","params":[],"id":11}' | jq -r '.result')"
  [[ "$connected" == "true" ]] && return 0
  host_id="$(deluge_rpc_checked '{"method":"web.get_hosts","params":[],"id":12}' | jq -r '.result[0][0] // empty')"
  if [[ -z "$host_id" ]]; then
    deluge_rpc_checked '{"method":"web.add_host","params":["127.0.0.1",58846,"",""],"id":13}' >/dev/null
    host_id="$(deluge_rpc_checked '{"method":"web.get_hosts","params":[],"id":14}' | jq -r '.result[0][0] // empty')"
  fi
  [[ -n "$host_id" ]]
  deluge_rpc_checked "{\"method\":\"web.connect\",\"params\":[\"$host_id\"],\"id\":15}" >/dev/null
}

add_deluge() {
  local torrent="$1" save_path="$2" data name payload
  [[ -r "$torrent" ]] || { log "Deluge torrent file is not readable: $(printf '%q' "$torrent")"; return 1; }
  deluge_connect
  data="$(base64 -w0 "$torrent")"
  name="$(basename "$torrent")"
  payload="$(mktemp "$WORKDIR/artifacts/deluge-add.XXXXXX.json")"
  printf '{"method":"core.add_torrent_file","params":["%s","%s",{"download_location":"%s"}],"id":2}\n' "$name" "$data" "$save_path" >"$payload"
  deluge_rpc_file_checked "$payload" >/dev/null
  rm -f "$payload"
}

add_rtorrent() {
  local torrent="$1" _save_path="$2"
  [[ -r "$torrent" ]] || { log "rTorrent torrent file is not readable: $(printf '%q' "$torrent")"; return 1; }
  copy_torrent_to_rtorrent_watch "$torrent"
}

add_to_client() {
  local client="$1" torrent="$2" add_mode="${3:-leecher}" save_path
  save_path="$(download_dir "$client")"
  case "$client" in
    torrentngd) add_rust "$torrent" "$save_path" ;;
    qbittorrent) add_qb "$torrent" "$save_path" "$add_mode" ;;
    transmission) add_transmission "$torrent" "$save_path" ;;
    deluge) add_deluge "$torrent" "$save_path" ;;
    rtorrent) add_rtorrent "$torrent" "$save_path" ;;
    *) return 1 ;;
  esac
}

client_progress() {
  local client="$1" fixture="${2:-}" name
  name="$(basename "$fixture")"
  case "$client" in
    torrentngd)
      curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/torrents/info" |
        jq -r --arg name "$name" '[.[] | select($name == "" or .name == $name) | .progress] | if length == 0 then 0 else min end'
      ;;
    qbittorrent)
      curl --max-time "$CURL_MAX_TIME" -fsS -H 'Host: localhost:8080' -b "$WORKDIR/artifacts/qbit.cookie" "$(client_url qbittorrent)/api/v2/torrents/info" |
        jq -r --arg name "$name" '[.[] | select($name == "" or .name == $name) | .progress] | if length == 0 then 0 else min end'
      ;;
    transmission)
      transmission_rpc '{"method":"torrent-get","arguments":{"fields":["name","percentDone"]}}' |
        jq -r --arg name "$name" '[.arguments.torrents[] | select($name == "" or .name == $name) | .percentDone] | if length == 0 then 0 else min end'
      ;;
    deluge)
      deluge_connect
      deluge_rpc_checked '{"method":"web.update_ui","params":[["name","progress"],{}],"id":3}' |
        jq -r --arg name "$name" '[.result.torrents[]? | select($name == "" or .name == $name) | .progress / 100] | if length == 0 then 0 else min end'
      ;;
    rtorrent)
      if [[ -n "$fixture" ]] && fixture_hashes_match rtorrent "$fixture"; then
        echo 1
      elif [[ -n "$name" ]] && rtorrent_public_complete "$name"; then
        echo 1
      else
        echo 0
      fi
      ;;
  esac
}

rtorrent_public_complete() {
  local expected_name="$1" torrent name total path size
  shopt -s nullglob
  for torrent in "$WORKDIR"/watch/rtorrent/*.torrent; do
    name="$(aria2c -S "$torrent" 2>/dev/null | awk -F': ' '/^Name: / {print $2; exit}')"
    [[ "$name" == "$expected_name" ]] || continue
    total="$(torrent_total_bytes "$torrent")"
    [[ -n "$name" && -n "$total" ]] || return 1
    path="$(host_download_dir rtorrent)/$name"
    [[ -f "$path" ]] || path="$(host_download_dir rtorrent)/public/$name"
    if [[ -f "$path" ]]; then
      size="$(stat -c '%s' "$path" 2>/dev/null || echo 0)"
    elif [[ -d "$path" ]]; then
      size="$(find "$path" -type f -printf '%s\n' 2>/dev/null | awk '{sum += $1} END {print sum + 0}')"
    else
      return 1
    fi
    [[ "$size" -ge "$total" ]] || return 1
    shopt -u nullglob
    return 0
  done
  shopt -u nullglob
  return 1
}

poll_rust_compat() {
  local out="$WORKDIR/artifacts/rust-api-poll-$STAMP.jsonl"
  {
    printf '{"endpoint":"health","ok":'
    curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/health" >/dev/null && printf 'true}\n' || printf 'false}\n'
    printf '{"endpoint":"metrics","ok":'
    curl --max-time "$CURL_MAX_TIME" -fsS "$(client_url torrentngd)/metrics" >/dev/null && printf 'true}\n' || printf 'false}\n'
    printf '{"endpoint":"qbit_info","ok":'
    curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/torrents/info" >/dev/null && printf 'true}\n' || printf 'false}\n'
    printf '{"endpoint":"qbit_sync","ok":'
    curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/sync/maindata" >/dev/null && printf 'true}\n' || printf 'false}\n'
    printf '{"endpoint":"qbit_transfer","ok":'
    curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/transfer/info" >/dev/null && printf 'true}\n' || printf 'false}\n'
    printf '{"endpoint":"transmission_stats","ok":'
    rust_transmission_rpc '{"method":"session-stats"}' >/dev/null 2>&1 && printf 'true}\n' || printf 'false}\n'
    printf '{"endpoint":"deluge_ui","ok":'
    rust_deluge_rpc '{"method":"web.update_ui","params":[["progress"],{}],"id":10}' >/dev/null 2>&1 && printf 'true}\n' || printf 'false}\n'
  } >>"$out"
}

rust_transmission_rpc() {
  local body="$1" url sid
  url="$(client_url torrentngd)/transmission/rpc"
  sid="$(curl --max-time "$CURL_MAX_TIME" -sS -D - -o /dev/null -H "Authorization: Bearer $RUST_TOKEN" -H "Content-Type: application/json" -d "$body" "$url" | awk 'tolower($0) ~ /^x-transmission-session-id:/ {print $2}' | tr -d '\r')"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" -H "X-Transmission-Session-Id: $sid" -H "Content-Type: application/json" -d "$body" "$url"
}

rust_deluge_rpc() {
  local body="$1"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" -H "Content-Type: application/json" -d "$body" "$(client_url torrentngd)/json"
}

wait_clients_complete() {
  local timeout="$1" fixture="$2"; shift 2
  local clients=("$@") start now progress
  start="$(date +%s)"
  while true; do
    poll_rust_compat || true
    local all_done=1
    for client in "${clients[@]}"; do
      progress="$(client_progress "$client" "$fixture" 2>/dev/null || echo 0)"
      awk -v p="$progress" 'BEGIN { exit !(p >= 0.999) }' || all_done=0
    done
    [[ "$all_done" == "1" ]] && return 0
    now="$(date +%s)"
    if (( now - start > timeout )); then
      return 1
    fi
    sleep 10
  done
}

wait_explicit_peer_complete() {
  local timeout="$1" fixture="$2" info_hash="$3" peer_client="$4"; shift 4
  local clients=("$@") start now progress last_bridge=0
  start="$(date +%s)"
  while true; do
    now="$(date +%s)"
    if (( now - last_bridge >= ${INTEROP_EXPLICIT_PEER_REFRESH_SECS:-30} )); then
      bridge_client_peer_to_rust "$peer_client" "$info_hash" || true
      last_bridge="$now"
    fi
    poll_rust_compat || true
    local all_done=1
    for client in "${clients[@]}"; do
      progress="$(client_progress "$client" "$fixture" 2>/dev/null || echo 0)"
      awk -v p="$progress" 'BEGIN { exit !(p >= 0.999) }' || all_done=0
    done
    [[ "$all_done" == "1" ]] && return 0
    if (( now - start > timeout )); then
      return 1
    fi
    sleep 5
  done
}

rust_observed_peers() {
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/sync/torrentPeers?hash=$1" |
    jq '.peers | length' 2>/dev/null || echo 0
}

torrent_info_hash() {
  aria2c -S "$1" 2>/dev/null | awk -F': ' '/^Info Hash: / {print tolower($2); exit}'
}

torrent_name() {
  aria2c -S "$1" 2>/dev/null | awk -F': ' '/^Name: / {print $2; exit}'
}

urlencode() {
  jq -nr --arg v "$1" '$v|@uri'
}

bridge_client_peer_to_rust() {
  local client="$1" info_hash="$2" ip port
  ip="$(container_ip "$client")"
  case "$client" in
    qbittorrent) port=6882 ;;
    transmission) port=51413 ;;
    deluge) port=6884 ;;
    rtorrent) port=6885 ;;
    *) return 1 ;;
  esac
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hashes=$info_hash" \
    --data-urlencode "peers=$ip:$port" \
    "$(client_url torrentngd)/api/qb/v2/torrents/addPeers" >/dev/null
}

wait_public_complete() {
  local timeout="$1" torrent_name="$2" info_hash="$3"; shift 3
  local clients=("$@") start now rust_progress rust_peers progress
  start="$(date +%s)"
  while true; do
    poll_rust_compat || true

    local all_done=1
    for client in "${clients[@]}"; do
      progress="$(client_progress "$client" "$torrent_name" 2>/dev/null || echo 0)"
      awk -v p="$progress" 'BEGIN { exit !(p >= 0.999) }' || all_done=0
    done
    [[ "$all_done" == "1" ]] && return 0

    rust_progress="$(client_progress torrentngd "$torrent_name" 2>/dev/null || echo 0)"
    rust_peers="$(rust_observed_peers "$info_hash")"
    if awk -v p="$rust_progress" 'BEGIN { exit !(p >= 0.999) }' &&
      [[ "${rust_peers:-0}" -ge "$PUBLIC_MIN_RUST_PEERS" ]]; then
      return 0
    fi

    now="$(date +%s)"
    if (( now - start > timeout )); then
      return 1
    fi
    sleep 10
  done
}

fixture_hashes_match() {
  local client="$1" fixture="$2" expected actual
  [[ -d "$WORKDIR/fixtures/$fixture" ]] || return 1
  [[ -d "$(host_download_dir "$client")/$fixture" ]] || return 1
  expected="$(mktemp)"
  actual="$(mktemp)"
  (cd "$WORKDIR/fixtures/$fixture" && find . -type f -print0 | sort -z | xargs -0 sha256sum) >"$expected"
  (cd "$(host_download_dir "$client")/$fixture" && find . -type f -print0 | sort -z | xargs -0 sha256sum) >"$actual"
  local status=0
  diff -u "$expected" "$actual" >/dev/null || status=1
  rm -f "$expected" "$actual"
  return "$status"
}

verify_fixture_hashes() {
  local client="$1" fixture="$2" expected actual
  [[ -d "$(host_download_dir "$client")/$fixture" ]] || return 1
  expected="$WORKDIR/artifacts/expected-$fixture.sha256"
  actual="$WORKDIR/artifacts/actual-$client-$fixture.sha256"
  mkdir -p "$(dirname "$expected")" "$(dirname "$actual")"
  (cd "$WORKDIR/fixtures/$fixture" && find . -type f -print0 | sort -z | xargs -0 sha256sum) >"$expected"
  (cd "$(host_download_dir "$client")/$fixture" && find . -type f -print0 | sort -z | xargs -0 sha256sum) >"$actual"
  diff -u "$expected" "$actual" >/dev/null
}

verify_selected_fixture_hashes() {
  local client="$1" fixture="$2" expected actual
  shift 2
  [[ -d "$(host_download_dir "$client")/$fixture" ]] || return 1
  expected="$WORKDIR/artifacts/expected-$fixture-selected.sha256"
  actual="$WORKDIR/artifacts/actual-$client-$fixture-selected.sha256"
  : >"$expected"
  : >"$actual"
  for rel in "$@"; do
    [[ -f "$WORKDIR/fixtures/$fixture/$rel" ]] || return 1
    [[ -f "$(host_download_dir "$client")/$fixture/$rel" ]] || return 1
    (cd "$WORKDIR/fixtures/$fixture" && sha256sum "$rel") >>"$expected" || return 1
    (cd "$(host_download_dir "$client")/$fixture" && sha256sum "$rel") >>"$actual" || return 1
  done
  diff -u "$expected" "$actual" >/dev/null
}

verify_fixture_file_absent_or_empty() {
  local client="$1" fixture="$2" rel="$3" path
  path="$(host_download_dir "$client")/$fixture/$rel"
  [[ ! -e "$path" ]] || [[ -f "$path" && ! -s "$path" ]]
}

run_local_case() {
  local row="$1" name seeder leecher fixture torrent torrent_fixture clients=()
  IFS='|' read -r name seeder leecher fixture <<<"$row"
  append_report "## Local: $name"
  append_report ""
  log "running local case $name"

  if [[ "$fixture" == "churn" ]]; then
    run_churn_case
    return
  fi

  if [[ "$fixture" == single-* ]]; then
    torrent_fixture="$(case_fixture "$fixture" "$name")"
  else
    torrent_fixture="$fixture"
  fi
  torrent="$(make_torrent "$torrent_fixture" "$torrent_fixture")"
  if [[ "$seeder" == "all" ]]; then
    clients=("${CLIENTS[@]}")
    for client in "${CLIENTS[@]}"; do
      seed_fixture_for_client "$client" "$torrent_fixture"
      add_to_client "$client" "$torrent" seed
    done
  else
    seed_fixture_for_client "$seeder" "$torrent_fixture"
    add_to_client "$seeder" "$torrent" seed
    add_to_client "$leecher" "$torrent"
    clients=("$seeder" "$leecher")
  fi

  local status="PASS"
  if ! wait_clients_complete "$TIMEOUT_LOCAL" "$torrent_fixture" "${clients[@]}"; then
    status="FAIL"
  fi
  for client in "${clients[@]}"; do
    if ! verify_fixture_hashes "$client" "$torrent_fixture"; then
      status="FAIL"
    fi
  done

  append_report "- Seeder: $seeder"
  append_report "- Leecher: $leecher"
  append_report "- Fixture: $fixture"
  append_report "- Torrent: \`$torrent\`"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_churn_case() {
  local status="PASS" i fixture torrent seeder leecher
  for i in $(seq -w 0 24); do
    fixture="churn/churn-$i.bin"
    torrent="$(make_torrent "$fixture" "churn-$i")"
    seeder="${CLIENTS[$((10#$i % ${#CLIENTS[@]}))]}"
    leecher="${CLIENTS[$(((10#$i + 1) % ${#CLIENTS[@]}))]}"
    seed_fixture_for_client "$seeder" "churn"
    add_to_client "$seeder" "$torrent" seed || status="FAIL"
    add_to_client "$leecher" "$torrent" || status="FAIL"
  done
  sleep "${INTEROP_CHURN_SETTLE_SECS:-30}"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/health" >/dev/null || status="FAIL"
  local service service_id
  for service in torrentngd qbittorrent transmission deluge rtorrent opentracker fixture-http; do
    service_id="$(compose ps -q "$service")"
    [[ -n "$service_id" ]] && [[ "$(docker inspect -f '{{.State.Running}}' "$service_id")" == "true" ]] || status="FAIL"
  done
  append_report "- Fixture count: 25"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_webseed_only_case() {
  local status="PASS" fixture torrent
  append_report "## Extended Local: rust-webseed-only"
  append_report ""
  log "running extended local case rust-webseed-only"
  fixture="$(case_fixture single-16m rust-webseed-only)"
  torrent="$(make_torrent "$fixture" "$fixture" webseed-only)"
  add_to_client torrentngd "$torrent" || status="FAIL"
  wait_clients_complete "$TIMEOUT_LOCAL" "$fixture" torrentngd || status="FAIL"
  verify_fixture_hashes torrentngd "$fixture" || status="FAIL"
  append_report "- Seeder: fixture-http"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: single-16m"
  append_report "- Torrent mode: webseed-only"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_explicit_peer_case() {
  local status="PASS" fixture torrent info_hash
  append_report "## Extended Local: rust-explicit-peer-private"
  append_report ""
  log "running extended local case rust-explicit-peer-private"
  fixture="$(case_fixture single-16m rust-explicit-peer-private)"
  torrent="$(make_torrent "$fixture" "$fixture" private-explicit)"
  info_hash="$(torrent_info_hash "$torrent")"
  seed_fixture_for_client transmission "$fixture"
  add_to_client transmission "$torrent" seed || status="FAIL"
  add_to_client torrentngd "$torrent" || status="FAIL"
  bridge_client_peer_to_rust transmission "$info_hash" || status="FAIL"
  wait_explicit_peer_complete "$TIMEOUT_LOCAL" "$fixture" "$info_hash" transmission transmission torrentngd || status="FAIL"
  verify_fixture_hashes transmission "$fixture" || status="FAIL"
  verify_fixture_hashes torrentngd "$fixture" || status="FAIL"
  append_report "- Seeder: transmission"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: single-16m"
  append_report "- Torrent mode: private explicit peer, no tracker, no webseed"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_restart_recovery_case() {
  local status="PASS" fixture torrent info_hash
  append_report "## Extended Local: rust-restart-recovery"
  append_report ""
  log "running extended local case rust-restart-recovery"
  fixture="$(case_fixture single-64m rust-restart-recovery)"
  torrent="$(make_torrent "$fixture" "$fixture" tracker-webseed)"
  info_hash="$(torrent_info_hash "$torrent")"
  seed_fixture_for_client transmission "$fixture"
  add_to_client transmission "$torrent" seed || status="FAIL"
  add_to_client torrentngd "$torrent" || status="FAIL"
  bridge_client_peer_to_rust transmission "$info_hash" || true
  sleep "${INTEROP_RESTART_BEFORE_SECS:-5}"
  compose restart -t 20 torrentngd >/dev/null || status="FAIL"
  wait_http torrentngd "$(client_url torrentngd)/health" 120 || status="FAIL"
  bridge_client_peer_to_rust transmission "$info_hash" || true
  wait_explicit_peer_complete "$TIMEOUT_LOCAL" "$fixture" "$info_hash" transmission transmission torrentngd || status="FAIL"
  verify_fixture_hashes torrentngd "$fixture" || status="FAIL"
  append_report "- Seeder: transmission"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: single-64m"
  append_report "- Restart delay: ${INTEROP_RESTART_BEFORE_SECS:-5}s"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_rust_api_facade_case() {
  local status="PASS"
  append_report "## Extended Local: rust-api-facades"
  append_report ""
  log "running extended local case rust-api-facades"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/health" |
    jq -e '.ready == true and .status == "ok"' >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS "$(client_url torrentngd)/metrics" |
    grep -q '^# HELP' || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/v1/torrents" |
    jq -e 'type == "array"' >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/torrents/info" |
    jq -e 'type == "array"' >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/sync/maindata" |
    jq -e 'type == "object" and has("torrents")' >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/transfer/info" |
    jq -e 'type == "object"' >/dev/null || status="FAIL"
  rust_transmission_rpc '{"method":"session-stats"}' |
    jq -e '.result == "success"' >/dev/null || status="FAIL"
  rust_transmission_rpc '{"method":"torrent-get","arguments":{"fields":["id","name","percentDone"]}}' |
    jq -e '.result == "success" and (.arguments.torrents | type == "array")' >/dev/null || status="FAIL"
  rust_deluge_rpc '{"method":"web.update_ui","params":[["name","progress","state"],{}],"id":30}' |
    jq -e '.error == null and (.result.torrents | type == "object")' >/dev/null || status="FAIL"
  append_report "- Native REST: checked"
  append_report "- qBittorrent API: checked"
  append_report "- Transmission RPC facade: checked"
  append_report "- Deluge JSON-RPC facade: checked"
  append_report "- Metrics: checked"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_magnet_tracker_case() {
  local status="PASS" fixture torrent info_hash name magnet
  append_report "## Protocol Local: rust-magnet-with-tracker"
  append_report ""
  log "running protocol local case rust-magnet-with-tracker"
  fixture="$(case_fixture single-16m rust-magnet-with-tracker)"
  torrent="$(make_torrent "$fixture" "$fixture" tracker-only)"
  info_hash="$(torrent_info_hash "$torrent")"
  name="$(torrent_name "$torrent")"
  magnet="magnet:?xt=urn:btih:$info_hash&dn=$(urlencode "$name")&tr=$(urlencode 'http://opentracker:6969/announce')"
  seed_fixture_for_client qbittorrent "$fixture"
  add_to_client qbittorrent "$torrent" seed || status="FAIL"
  qb_force_start "$info_hash" || status="FAIL"
  add_rust_url "$magnet" "$(download_dir torrentngd)" || status="FAIL"
  bridge_client_peer_to_rust qbittorrent "$info_hash" || true
  wait_explicit_peer_complete "$TIMEOUT_LOCAL" "$fixture" "$info_hash" qbittorrent qbittorrent torrentngd || status="FAIL"
  verify_fixture_hashes torrentngd "$fixture" || status="FAIL"
  append_report "- Seeder: qbittorrent"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: single-16m"
  append_report "- Add method: magnet URL with HTTP tracker"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_udp_tracker_case() {
  local status="PASS" fixture torrent info_hash
  append_report "## Protocol Local: rust-udp-tracker"
  append_report ""
  log "running protocol local case rust-udp-tracker"
  fixture="$(case_fixture single-16m rust-udp-tracker)"
  torrent="$(make_torrent "$fixture" "$fixture" udp-tracker-only)"
  info_hash="$(torrent_info_hash "$torrent")"
  seed_fixture_for_client transmission "$fixture"
  add_to_client transmission "$torrent" seed || status="FAIL"
  add_to_client torrentngd "$torrent" || status="FAIL"
  wait_clients_complete "$TIMEOUT_LOCAL" "$fixture" transmission torrentngd || status="FAIL"
  verify_fixture_hashes torrentngd "$fixture" || status="FAIL"
  append_report "- Seeder: transmission"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: single-16m"
  append_report "- Tracker: udp://opentracker:6969/announce"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_multi_tracker_fallback_case() {
  local status="PASS" fixture torrent info_hash
  append_report "## Protocol Local: rust-multi-tracker-fallback"
  append_report ""
  log "running protocol local case rust-multi-tracker-fallback"
  fixture="$(case_fixture single-16m rust-multi-tracker-fallback)"
  torrent="$(make_torrent "$fixture" "$fixture" multi-tracker-fallback)"
  info_hash="$(torrent_info_hash "$torrent")"
  seed_fixture_for_client transmission "$fixture"
  add_to_client transmission "$torrent" seed || status="FAIL"
  add_to_client torrentngd "$torrent" || status="FAIL"
  wait_clients_complete "$TIMEOUT_LOCAL" "$fixture" transmission torrentngd || status="FAIL"
  verify_fixture_hashes torrentngd "$fixture" || status="FAIL"
  append_report "- Seeder: transmission"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: single-16m"
  append_report "- First tracker: http://127.0.0.1:9/dead-announce"
  append_report "- Fallback tracker: http://opentracker:6969/announce"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_partial_file_selection_case() {
  local status="PASS" fixture torrent info_hash files
  append_report "## Protocol Local: rust-partial-file-selection"
  append_report ""
  log "running protocol local case rust-partial-file-selection"
  fixture="$(case_fixture multi-128m rust-partial-file-selection)"
  torrent="$(make_torrent "$fixture" "$fixture" private-explicit)"
  info_hash="$(torrent_info_hash "$torrent")"
  seed_fixture_for_client transmission "$fixture"
  add_to_client transmission "$torrent" seed || status="FAIL"
  add_to_client torrentngd "$torrent" || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hash=$info_hash" \
    --data-urlencode "id=0" \
    --data-urlencode "priority=0" \
    "$(client_url torrentngd)/api/qb/v2/torrents/filePrio" >/dev/null || status="FAIL"
  bridge_client_peer_to_rust transmission "$info_hash" || true
  wait_explicit_peer_complete "$TIMEOUT_LOCAL" "$fixture" "$info_hash" transmission transmission torrentngd || status="FAIL"
  verify_selected_fixture_hashes torrentngd "$fixture" \
    ./part-1.bin ./part-2.bin ./part-3.bin ./part-4.bin \
    ./part-5.bin ./part-6.bin ./part-7.bin || status="FAIL"
  verify_fixture_file_absent_or_empty torrentngd "$fixture" part-0.bin || status="FAIL"
  files="$(curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/torrents/files?hash=$info_hash" || true)"
  jq -e '. | length == 8 and .[0].priority == 0 and .[0].progress == 1 and ([.[1:][] | select(.priority > 0)] | length == 7)' <<<"$files" >/dev/null || status="FAIL"
  append_report "- Seeder: transmission"
  append_report "- Leecher: torrentngd"
  append_report "- Fixture: multi-128m"
  append_report "- Torrent mode: private explicit peer, file 0 skipped"
  append_report "- Wanted files: part-1.bin through part-7.bin"
  append_report "- Skipped file: part-0.bin absent or empty"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_qbit_mutation_facade_case() {
  local status="PASS" fixture torrent info_hash original replacement trackers
  append_report "## Protocol Local: rust-qbit-mutation-facade"
  append_report ""
  log "running protocol local case rust-qbit-mutation-facade"
  fixture="$(case_fixture multi-128m rust-qbit-mutation-facade)"
  torrent="$(make_torrent "$fixture" "$fixture" tracker-only)"
  info_hash="$(torrent_info_hash "$torrent")"
  original="http://opentracker:6969/announce"
  replacement="http://opentracker:6969/announce?tng=edited"
  add_to_client torrentngd "$torrent" || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hash=$info_hash" \
    --data-urlencode "id=0" \
    --data-urlencode "priority=0" \
    "$(client_url torrentngd)/api/qb/v2/torrents/filePrio" >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hashes=$info_hash" \
    "$(client_url torrentngd)/api/qb/v2/torrents/recheck" >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hash=$info_hash" \
    --data-urlencode "urls=http://127.0.0.1:9/dead-announce" \
    "$(client_url torrentngd)/api/qb/v2/torrents/addTrackers" >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hash=$info_hash" \
    --data-urlencode "origUrl=$original" \
    --data-urlencode "newUrl=$replacement" \
    "$(client_url torrentngd)/api/qb/v2/torrents/editTracker" >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hash=$info_hash" \
    --data-urlencode "urls=http://127.0.0.1:9/dead-announce" \
    "$(client_url torrentngd)/api/qb/v2/torrents/removeTrackers" >/dev/null || status="FAIL"
  trackers="$(curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/torrents/trackers?hash=$info_hash" || true)"
  jq -e --arg replacement "$replacement" '[.[].url] | index($replacement) != null' <<<"$trackers" >/dev/null || status="FAIL"
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" "$(client_url torrentngd)/api/qb/v2/torrents/files?hash=$info_hash" |
    jq -e 'type == "array" and length >= 1 and .[0].priority == 0' >/dev/null || status="FAIL"
  append_report "- Target: torrentngd qBittorrent-compatible mutation endpoints"
  append_report "- Checked: filePrio, recheck, addTrackers, editTracker, removeTrackers, trackers, files"
  append_report "- Fixture: multi-128m"
  append_report "- Info hash: $info_hash"
  append_report "- Status: **$status**"
  append_report ""
  [[ "$status" == "PASS" ]]
}

run_protocol_local_matrix() {
  local failures=0
  [[ "${INTEROP_PROTOCOL_LOCAL:-1}" == "1" ]] || return 0
  append_report "# Protocol Local Certification"
  append_report ""
  if [[ -z "${INTEROP_PROTOCOL_ONLY:-}" || "${INTEROP_PROTOCOL_ONLY:-}" == "rust-magnet-with-tracker" ]]; then
    run_magnet_tracker_case || failures=$((failures + 1))
  fi
  if [[ -z "${INTEROP_PROTOCOL_ONLY:-}" || "${INTEROP_PROTOCOL_ONLY:-}" == "rust-udp-tracker" ]]; then
    run_udp_tracker_case || failures=$((failures + 1))
  fi
  if [[ -z "${INTEROP_PROTOCOL_ONLY:-}" || "${INTEROP_PROTOCOL_ONLY:-}" == "rust-multi-tracker-fallback" ]]; then
    run_multi_tracker_fallback_case || failures=$((failures + 1))
  fi
  if [[ -z "${INTEROP_PROTOCOL_ONLY:-}" || "${INTEROP_PROTOCOL_ONLY:-}" == "rust-partial-file-selection" ]]; then
    run_partial_file_selection_case || failures=$((failures + 1))
  fi
  if [[ -z "${INTEROP_PROTOCOL_ONLY:-}" || "${INTEROP_PROTOCOL_ONLY:-}" == "rust-qbit-mutation-facade" ]]; then
    run_qbit_mutation_facade_case || failures=$((failures + 1))
  fi
  (( failures == 0 ))
}

run_extended_local_matrix() {
  local failures=0
  [[ "$EXTENDED_LOCAL" == "1" ]] || return 0
  append_report "# Extended Local Certification"
  append_report ""
  run_webseed_only_case || failures=$((failures + 1))
  run_explicit_peer_case || failures=$((failures + 1))
  run_restart_recovery_case || failures=$((failures + 1))
  run_rust_api_facade_case || failures=$((failures + 1))
  (( failures == 0 ))
}

toml_entries() {
  awk '
    /^\[\[torrent\]\]/ { if (id) print id "|" enabled "|" source "|" resolver "|" pattern "|" max "|" clients; id=enabled=source=resolver=pattern=max=clients="" }
    /^id = / { gsub(/"/, "", $3); id=$3 }
    /^enabled = / { enabled=$3 }
    /^source_url = / { source=$0; sub(/^source_url = "/, "", source); sub(/"$/, "", source) }
    /^resolver_url = / { resolver=$0; sub(/^resolver_url = "/, "", resolver); sub(/"$/, "", resolver) }
    /^resolver_pattern = / { pattern=$0; sub(/^resolver_pattern = "/, "", pattern); sub(/"$/, "", pattern); gsub(/\\\\/, "\\", pattern) }
    /^max_runtime_secs = / { max=$3 }
    /^clients = / { clients=$0; sub(/^clients = \[/, "", clients); sub(/\]$/, "", clients); gsub(/[",]/, "", clients) }
    END { if (id) print id "|" enabled "|" source "|" resolver "|" pattern "|" max "|" clients }
  ' "$PUBLIC_TOML"
}

resolve_public_torrent() {
  local id="$1" resolver="$2" pattern="$3" html url
  if [[ "$id" == "ubuntu" ]]; then
    local lts_dir
    lts_dir="$(curl --max-time "$CURL_MAX_TIME" -fsSL "$resolver" | grep -Eo 'href="[0-9]+\.04(\.[0-9]+)?/"' | sed -E 's/^href="([^"]+)".*/\1/' | sort -V | tail -n1)"
    [[ -n "$lts_dir" ]] || return 1
    resolver="${resolver%/}/$lts_dir"
  elif [[ "$id" == "libreoffice" ]]; then
    local stable_dir
    stable_dir="$(curl --max-time "$CURL_MAX_TIME" -fsSL "$resolver" | grep -Eo 'href="[0-9]+(\.[0-9]+)+/"' | sed -E 's/^href="([^"]+)".*/\1/' | sort -V | tail -n1)"
    [[ -n "$stable_dir" ]] || return 1
    resolver="${resolver%/}/$stable_dir/deb/x86_64/"
  fi
  html="$(curl --max-time "$CURL_MAX_TIME" -fsSL "$resolver")"
  url="$(printf '%s' "$html" | grep -Eo "$pattern" | sort -V | tail -n1 || true)"
  [[ -n "$url" ]] || return 1
  if [[ "$url" =~ ^https?:// ]]; then
    echo "$url"
  else
    printf '%s/%s\n' "${resolver%/}" "$url"
  fi
}

add_public_to_client() {
  local client="$1" url="$2" torrent_file="${3:-}" save_path
  save_path="$(download_dir "$client")/public"
  case "$client" in
    torrentngd)
      curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" -F "urls=$url" -F "savepath=$save_path" "$(client_url torrentngd)/api/qb/v2/torrents/add" >/dev/null
      ;;
    qbittorrent)
      qb_login
      curl --max-time "$CURL_MAX_TIME" -fsS -H 'Host: localhost:8080' -b "$WORKDIR/artifacts/qbit.cookie" -F "urls=$url" -F "savepath=$save_path" "$(client_url qbittorrent)/api/v2/torrents/add" >/dev/null
      ;;
    transmission)
      transmission_rpc "{\"method\":\"torrent-add\",\"arguments\":{\"filename\":\"$url\",\"download-dir\":\"$save_path\",\"paused\":false}}" >/dev/null
      ;;
    deluge)
      if [[ -n "$torrent_file" ]]; then
        add_deluge "$torrent_file" "$save_path"
      else
        deluge_connect
        deluge_rpc_checked "{\"method\":\"core.add_torrent_url\",\"params\":[\"$url\",{\"download_location\":\"$save_path\"}],\"id\":4}" >/dev/null
      fi
      ;;
    rtorrent)
      if [[ -n "$torrent_file" ]]; then
        cp "$torrent_file" "$WORKDIR/watch/rtorrent/$client-$(basename "$url")"
      else
        curl --max-time "$CURL_MAX_TIME" -fsSL "$url" -o "$WORKDIR/watch/rtorrent/$client-$(basename "$url")"
      fi
      ;;
  esac
}

public_torrent_metadata() {
  local id="$1" url="$2" torrent_file name total info_hash
  torrent_file="$WORKDIR/torrents/public-$id.torrent"
  curl --max-time "$CURL_MAX_TIME" -fsSL "$url" -o "$torrent_file"
  name="$(aria2c -S "$torrent_file" 2>/dev/null | awk -F': ' '/^Name: / {print $2; exit}')"
  total="$(torrent_total_bytes "$torrent_file")"
  info_hash="$(aria2c -S "$torrent_file" 2>/dev/null | awk -F': ' '/^Info Hash: / {print tolower($2); exit}')"
  [[ -n "$name" && -n "$total" && -n "$info_hash" ]] || return 1
  printf '%s|%s|%s|%s\n' "$torrent_file" "$name" "$total" "$info_hash"
}

torrent_total_bytes() {
  aria2c -S "$1" 2>/dev/null |
    sed -nE '/^Total Length:/ { s/.*\(([0-9,]+)\).*/\1/; s/,//g; p; q; }'
}

container_ip() {
  local service="$1" cid
  cid="$(compose ps -q "$service")"
  [[ -n "$cid" ]] || return 1
  docker inspect -f '{{range .NetworkSettings.Networks}}{{.IPAddress}}{{end}}' "$cid"
}

public_bridge_peers() {
  local peers=() ip
  if ip="$(container_ip qbittorrent 2>/dev/null)" && [[ -n "$ip" ]]; then
    peers+=("$ip:6882")
  fi
  if ip="$(container_ip transmission 2>/dev/null)" && [[ -n "$ip" ]]; then
    peers+=("$ip:51413")
  fi
  if ip="$(container_ip deluge 2>/dev/null)" && [[ -n "$ip" ]]; then
    peers+=("$ip:6884")
  fi
  if ip="$(container_ip rtorrent 2>/dev/null)" && [[ -n "$ip" ]]; then
    peers+=("$ip:6885")
  fi
  (IFS='|'; printf '%s\n' "${peers[*]}")
}

bridge_public_reference_peers_to_rust() {
  local info_hash="$1" peers
  peers="$(public_bridge_peers)"
  [[ -n "$peers" ]] || return 0
  curl --max-time "$CURL_MAX_TIME" -fsS -H "Authorization: Bearer $RUST_TOKEN" \
    --data-urlencode "hashes=$info_hash" \
    --data-urlencode "peers=$peers" \
    "$(client_url torrentngd)/api/qb/v2/torrents/addPeers" >/dev/null
  append_report "- Docker reference peers bridged to Rust: $peers"
}

run_public_entry() {
  local entry="$1" id enabled source resolver pattern max clients url metadata torrent_file torrent_name total info_hash status="PASS"
  IFS='|' read -r id enabled source resolver pattern max clients <<<"$entry"
  local optional=false
  if [[ -n "${INTEROP_PUBLIC_ONLY:-}" && "$id" != "$INTEROP_PUBLIC_ONLY" ]]; then
    return 0
  fi
  if [[ "$enabled" != "true" ]]; then
    [[ "$id" == "libreoffice" && "${INTEROP_INCLUDE_LIBREOFFICE:-0}" == "1" ]] || return 0
    optional=true
  fi
  append_report "## Public: $id"
  append_report ""
  append_report "- Source: $source"
  log "resolving public torrent $id from official source"
  if ! url="$(resolve_public_torrent "$id" "$resolver" "$pattern")"; then
    if [[ "$optional" == "true" ]]; then
      append_report "- Status: **SKIP**"
      append_report "- Reason: optional official source did not publish a matching torrent"
      append_report ""
      return 0
    fi
    append_report "- Status: **RESOLVER FAIL**"
    append_report ""
    return 1
  fi
  append_report "- Resolved torrent: $url"
  if ! metadata="$(public_torrent_metadata "$id" "$url")"; then
    append_report "- Status: **METADATA FAIL**"
    append_report ""
    return 1
  fi
  IFS='|' read -r torrent_file torrent_name total info_hash <<<"$metadata"
  append_report "- Torrent name: $torrent_name"
  append_report "- Info hash: $info_hash"
  append_report "- Total bytes: $total"

  local selected=()
  read -r -a selected <<<"$clients"
  for client in "${selected[@]}"; do
    add_public_to_client "$client" "$url" "$torrent_file" || status="FAIL"
  done
  bridge_public_reference_peers_to_rust "$info_hash" || status="FAIL"
  if ! wait_public_complete "${max:-$TIMEOUT_PUBLIC}" "$torrent_name" "$info_hash" "${selected[@]}"; then
    status="FAIL"
  fi

  local rust_peers
  rust_peers="$(rust_observed_peers "$info_hash")"
  append_report "- Rust peer observation floor: $PUBLIC_MIN_RUST_PEERS"
  append_report "- Rust peers observed: $rust_peers"
  append_report "- Clients: ${selected[*]}"
  append_report "- Status: **$status**"
  append_report ""

  [[ "$status" == "PASS" ]]
}

cleanup_public_data() {
  if [[ "$KEEP_PUBLIC_DATA" == "1" ]]; then
    return 0
  fi
  chmod -R u+rwX "$WORKDIR/downloads" 2>/dev/null || true
  for client in "${CLIENTS[@]}"; do
    rm -rf "$(host_download_dir "$client")/public" 2>/dev/null || true
  done
  rm -rf "$(host_download_dir rtorrent)"/*.iso "$(host_download_dir rtorrent)"/*.img "$(host_download_dir rtorrent)"/Fedora-* 2>/dev/null || true
  if find "$WORKDIR/downloads" \( -path '*/public/*' -o -name '*.iso' -o -name '*.img' \) 2>/dev/null | grep -q .; then
    docker run --rm -v "$WORKDIR/downloads:/downloads" alpine:3.20 sh -lc \
      "rm -rf /downloads/*/public /downloads/rtorrent/*.iso /downloads/rtorrent/*.img /downloads/rtorrent/Fedora-*" >/dev/null 2>&1 || true
  fi
}

run_public_matrix() {
  local failures=0 running=0 pids=()
  append_report "# Public Legal Torrent Matrix"
  append_report ""
  if (( PUBLIC_MAX_PARALLEL <= 1 )); then
    while IFS= read -r entry; do
      run_public_entry "$entry" || failures=$((failures + 1))
    done < <(toml_entries)
    cleanup_public_data
    (( failures == 0 ))
    return
  fi
  while IFS= read -r entry; do
    while (( running >= PUBLIC_MAX_PARALLEL )); do
      wait -n || failures=$((failures + 1))
      running=$((running - 1))
    done
    run_public_entry "$entry" &
    pids+=("$!")
    running=$((running + 1))
  done < <(toml_entries)
  for pid in "${pids[@]}"; do
    wait "$pid" || failures=$((failures + 1))
  done
  cleanup_public_data
  (( failures == 0 ))
}

run_local_matrix() {
  local failures=0
  append_report "# Deterministic Local Swarm"
  append_report ""
  create_fixture_files
  if [[ "${INTEROP_EXTENDED_ONLY:-0}" != "1" ]]; then
    for row in "${LOCAL_CASES[@]}"; do
      run_local_case "$row" || failures=$((failures + 1))
    done
  fi
  run_extended_local_matrix || failures=$((failures + 1))
  run_protocol_local_matrix || failures=$((failures + 1))
  (( failures == 0 ))
}

main() {
  require_cmd docker
  require_cmd curl
  require_cmd jq
  require_cmd base64

  if [[ "${INTEROP_REUSE_STACK:-0}" != "1" ]]; then
    compose down --remove-orphans -v >/dev/null 2>&1 || true
    reset_workdir
  fi
  prepare_dirs
  write_report_header
  trap cleanup EXIT

  log "starting interop compose stack"
  if [[ "${INTEROP_SKIP_BUILD:-0}" == "1" ]]; then
    compose up -d
  else
    compose up -d --build
  fi
  wait_stack

  local failed=0
  if [[ "$MODE" == "local" || "$MODE" == "all" ]]; then
    run_local_matrix || failed=1
  fi
  if [[ "$MODE" == "public" || "$MODE" == "all" ]]; then
    run_public_matrix || failed=1
  fi

  append_report "# Artifacts"
  append_report ""
  append_report "- Logs: \`$WORKDIR/logs/$STAMP\`"
  append_report "- Torrents: \`$WORKDIR/torrents\`"
  append_report "- API poll log: \`$WORKDIR/artifacts/rust-api-poll-$STAMP.jsonl\`"
  append_report ""

  if [[ "$failed" == "0" ]]; then
    append_report "**Overall: PASS**"
  else
    append_report "**Overall: FAIL**"
  fi
  log "wrote report $REPORT"
  return "$failed"
}

main "$@"
