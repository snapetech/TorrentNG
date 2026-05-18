#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/native-engine-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
LIVE_HEALTH_URL="${NATIVE_ENGINE_HEALTH_URL:-${NATIVE_ENGINE_URL:-}}"

if [[ -n "$LIVE_HEALTH_URL" && "$LIVE_HEALTH_URL" != */health ]]; then
  LIVE_HEALTH_URL="${LIVE_HEALTH_URL%/}/health"
fi

run_gate() {
  local name="$1"
  shift
  {
    echo
    echo "## $name"
    echo
    echo '```text'
  } >> "$OUT"
  if (cd "$ROOT" && "$@") >> "$OUT" 2>&1; then
    echo '```' >> "$OUT"
    printf '| %s | PASS |\n' "$name" >> "$OUT.table"
  else
    echo '```' >> "$OUT"
    printf '| %s | FAIL |\n' "$name" >> "$OUT.table"
    status="FAIL"
  fi
}

{
  echo "# TorrentNG Native Engine Certification Report"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Kernel: $(uname -srmo)"
  echo "- Rust: $(rustc --version 2>/dev/null || echo unavailable)"
  echo "- Cargo: $(cargo --version 2>/dev/null || echo unavailable)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo
  echo "## Gates"
  echo
  echo "| Gate | Result |"
  echo "|---|---|"
} > "$OUT"
: > "$OUT.table"

run_gate "universal compatibility certification" "$ROOT/scripts/universal_compatibility_certification.sh" "$ROOT/certification/reports/universal-compat-native-$(date -u +%Y%m%dT%H%M%SZ).md"

if [[ -n "$LIVE_HEALTH_URL" ]]; then
  # shellcheck disable=SC2016 # The inner script expands $1 and $body at runtime.
  run_gate "live native health capability manifest" bash -c '
    set -euo pipefail
    url="$1"
    body="$(curl -fsS "$url")"
    jq -e "
      .ready == true
      and .native_engine == true
      and .engine.track1_sidecar_required == false
      and .engine.source_of_truth == \"sqlite_session_db\"
      and .engine.capabilities.torrent_identity.v1 == true
      and .engine.capabilities.torrent_identity.v2 == true
      and .engine.capabilities.torrent_identity.hybrid == true
      and (.engine.capabilities.torrent_identity.hash_lengths == [40,64])
      and (.engine.capabilities.torrent_identity.magnet_xt | index(\"btih\") != null)
      and (.engine.capabilities.torrent_identity.magnet_xt | index(\"btmh\") != null)
      and .engine.capabilities.metadata.pure_v2_metadata_completion == true
      and .engine.capabilities.session.crash_restore == true
      and .engine.capabilities.jobs.durable_recheck == true
      and .engine.capabilities.storage.v2_file_root_verify == true
      and .engine.capabilities.networking.dht == true
      and .engine.capabilities.networking.utp_packet_codec == true
      and .engine.capabilities.networking.utp_transport == false
      and .engine.capabilities.compatibility.qbittorrent_v2 == true
      and .engine.capabilities.compatibility.transmission_rpc == true
      and .engine.capabilities.compatibility.deluge_rpc == true
      and .engine.capabilities.migration.rtorrent == true
      and .engine.capabilities.migration.qbittorrent == true
      and .engine.capabilities.migration.transmission == true
      and .engine.capabilities.operations.prometheus_metrics == true
      and .engine.capabilities.operations.diagnostics == true
    " <<<"$body"
  ' _ "$LIVE_HEALTH_URL"
else
  {
    echo
    echo "## live native health capability manifest"
    echo
    echo '```text'
    echo "SKIP: set NATIVE_ENGINE_URL or NATIVE_ENGINE_HEALTH_URL to assert a running daemon /health response."
    echo '```'
  } >> "$OUT"
  printf '| %s | SKIP |\n' "live native health capability manifest" >> "$OUT.table"
fi

sed -i "/|---|---|/r $OUT.table" "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
