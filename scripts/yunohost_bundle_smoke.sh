#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUNDLE_DIR="$(realpath "$1")"
CONFIG_WRITER="$ROOT_DIR/packaging/yunohost/conf/write-config.py"
RUNTIME_CONFIG_WRITER="$ROOT_DIR/packaging/yunohost/conf/write-runtime-config.py"
WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/torrentng-yunohost-smoke.XXXXXX")"
TOKEN="yunohost-smoke-token-0123456789abcdef"
NATIVE_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
PEER_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"
SIDECAR_PORT="$(python3 -c 'import socket; s=socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1]); s.close()')"

declare -a PIDS=()

cleanup() {
	for pid in "${PIDS[@]}"; do
		kill "$pid" 2>/dev/null || true
	done
	for pid in "${PIDS[@]}"; do
		wait "$pid" 2>/dev/null || true
	done
}
trap cleanup EXIT

mkdir -p "$WORK_DIR/native" "$WORK_DIR/downloads" "$WORK_DIR/sidecar"
TNG_DEPLOYMENT_MODE=native \
TNG_DATA_DIR="$WORK_DIR/native" \
TNG_HTTP_PORT="$NATIVE_PORT" \
TNG_PEER_PORT="$PEER_PORT" \
TNG_DOWNLOAD_DIR="$WORK_DIR/downloads" \
TNG_API_TOKEN="$TOKEN" \
	python3 "$CONFIG_WRITER" "$WORK_DIR/native/config.toml"

TNG_INSTALL_DIR="$BUNDLE_DIR" \
TNG_DATA_DIR="$WORK_DIR/native" \
TNG_DEPLOYMENT_MODE=native \
TNG_STATIC_DIR="$BUNDLE_DIR/webui" \
	"$ROOT_DIR/packaging/yunohost/scripts/launch" \
	>"$WORK_DIR/native.log" 2>&1 &
PIDS+=("$!")

wait_http() {
	local url="$1"
	local output="$2"
	local log="$3"
	for _ in $(seq 1 120); do
		if curl --noproxy '*' -fsS "$url" -o "$output" 2>/dev/null; then
			return 0
		fi
		sleep 0.25
	done
	cat "$log" >&2
	return 1
}

wait_http \
	"http://127.0.0.1:$NATIVE_PORT/health" \
	"$WORK_DIR/native-health.json" \
	"$WORK_DIR/native.log"
python3 -c 'import json, sys; assert json.load(open(sys.argv[1]))["ready"] is True' "$WORK_DIR/native-health.json"

cp -a "$BUNDLE_DIR/webui" "$WORK_DIR/webui"
python3 "$RUNTIME_CONFIG_WRITER" "$WORK_DIR/webui/runtime-config.js" /torrentng
TNG_DEPLOYMENT_MODE=existing \
TNG_DATA_DIR="$WORK_DIR/sidecar" \
TNG_HTTP_PORT="$SIDECAR_PORT" \
TNG_PEER_PORT="$PEER_PORT" \
TNG_DOWNLOAD_DIR="$WORK_DIR/sidecar" \
TNG_STORAGE_ROOT="$WORK_DIR/downloads" \
TNG_BACKEND_TYPE=torrentng \
TNG_SESSION_SECRET=yunohost-smoke-session-secret-0123456789abcdef \
TNG_API_TOKEN="$TOKEN" \
TNG_TORRENTNG_URL="http://127.0.0.1:$NATIVE_PORT" \
TNG_TORRENTNG_API_TOKEN="$TOKEN" \
	python3 "$CONFIG_WRITER" "$WORK_DIR/sidecar/config.toml"

TNG_INSTALL_DIR="$BUNDLE_DIR" \
TNG_DATA_DIR="$WORK_DIR/sidecar" \
TNG_DEPLOYMENT_MODE=existing \
TNG_STATIC_DIR="$WORK_DIR/webui" \
	"$ROOT_DIR/packaging/yunohost/scripts/launch" \
	>"$WORK_DIR/sidecar.log" 2>&1 &
PIDS+=("$!")
wait_http \
	"http://127.0.0.1:$SIDECAR_PORT/health" \
	"$WORK_DIR/sidecar-health.json" \
	"$WORK_DIR/sidecar.log"
python3 -c 'import json, sys; assert json.load(open(sys.argv[1]))["status"] == "ok"' "$WORK_DIR/sidecar-health.json"

curl --noproxy '*' -fsS "http://127.0.0.1:$SIDECAR_PORT/" -o "$WORK_DIR/index.html"
grep -q 'runtime-config.js' "$WORK_DIR/index.html"
grep -q 'assets/index-' "$WORK_DIR/index.html"
curl --noproxy '*' -fsS "http://127.0.0.1:$SIDECAR_PORT/runtime-config.js" \
	-o "$WORK_DIR/runtime-config.js"
grep -Fq 'window.__TNG_BASE_PATH__ = "/torrentng";' "$WORK_DIR/runtime-config.js"

login_status="$(curl --noproxy '*' -sS -o "$WORK_DIR/login.body" -D "$WORK_DIR/login.headers" -w '%{http_code}' \
	-H 'Content-Type: application/x-www-form-urlencoded' \
	--data-urlencode "username=$TOKEN" \
	--data-urlencode "password=$TOKEN" \
	"http://127.0.0.1:$SIDECAR_PORT/api/qb/v2/auth/login")"
[[ "$login_status" == "200" ]]
grep -q 'Ok\.' "$WORK_DIR/login.body"
grep -qi '^set-cookie: tng_session=.*Secure' "$WORK_DIR/login.headers"
SESSION_COOKIE="$(awk 'tolower($1) == "set-cookie:" && $2 ~ /^tng_session=/ { sub(/^tng_session=/, "", $2); sub(/;.*/, "", $2); print $2; exit }' "$WORK_DIR/login.headers")"
[[ -n "$SESSION_COOKIE" ]]
curl --noproxy '*' -fsS \
	-H "Cookie: tng_session=$SESSION_COOKIE" \
	"http://127.0.0.1:$SIDECAR_PORT/api/v1/transfer/info" \
	-o "$WORK_DIR/session-transfer.json"
curl --noproxy '*' -fsS \
	-H "Authorization: Bearer $TOKEN" \
	"http://127.0.0.1:$SIDECAR_PORT/api/v1/transfer/info" \
	-o "$WORK_DIR/token-transfer.json"

echo "YunoHost bundle smoke passed: native mode, existing-client sidecar, token login, protected API, and subpath WebUI config."
