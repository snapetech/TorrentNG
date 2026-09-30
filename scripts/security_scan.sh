#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT="${1:-$ROOT/certification/reports/security-scan-$(date -u +%Y%m%dT%H%M%SZ).md}"
IMAGE="${TNG_SCAN_IMAGE:-torrentng:certification}"
TMP_DIR="$(mktemp -d)"
trap 'rm -rf -- "$TMP_DIR"' EXIT

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
    npm audit --audit-level=high --omit=dev --json > "$TMP_DIR/npm-audit.json"
  ) >"$TMP_DIR/npm-audit.log" 2>&1 && npm_result=PASS || npm_result=FAIL
  high="$(jq -r '.metadata.vulnerabilities.high // 0' "$TMP_DIR/npm-audit.json" 2>/dev/null || echo unknown)"
  critical="$(jq -r '.metadata.vulnerabilities.critical // 0' "$TMP_DIR/npm-audit.json" 2>/dev/null || echo unknown)"
  if [[ "$npm_result" == "PASS" ]]; then
    mark "npm production audit" "PASS" "high=$high critical=$critical"
  else
    mark "npm production audit" "FAIL" "high=$high critical=$critical; see npm audit output"
  fi
else
  mark "npm production audit" "BLOCKED" "npm not installed"
fi

if command -v cargo >/dev/null 2>&1; then
  if (
    cd "$ROOT/sidecar"
    cargo tree --locked >"$TMP_DIR/cargo-tree.txt"
  ) >"$TMP_DIR/cargo-tree.log" 2>&1; then
    mark "cargo dependency tree" "PASS" "resolved with lockfile"
  else
    mark "cargo dependency tree" "FAIL" "cargo tree failed"
  fi
else
  mark "cargo dependency tree" "BLOCKED" "cargo not installed"
fi

if command -v cargo-audit >/dev/null 2>&1; then
  for audit_target in \
    "main:$ROOT/Cargo.lock" \
    "sidecar:$ROOT/sidecar/Cargo.lock" \
    "fuzz:$ROOT/fuzz/Cargo.lock"; do
    target_name="${audit_target%%:*}"
    lockfile="${audit_target#*:}"
    audit_log="$TMP_DIR/cargo-audit-$target_name.log"
    if cargo audit --file "$lockfile" >"$audit_log" 2>&1; then
      mark "RustSec audit ($target_name)" "PASS" "no actionable RustSec advisories"
    else
      cat "$audit_log" >&2
      mark "RustSec audit ($target_name)" "FAIL" "RustSec audit failed"
    fi
  done
else
  mark "RustSec audit (main)" "BLOCKED" "cargo-audit not installed; CI security-audit provides the release gate"
  mark "RustSec audit (sidecar)" "BLOCKED" "cargo-audit not installed; CI security-audit provides the release gate"
  mark "RustSec audit (fuzz)" "BLOCKED" "cargo-audit not installed; CI security-audit provides the release gate"
fi

if command -v docker >/dev/null 2>&1 && docker compose version >"$TMP_DIR/compose-version.txt" 2>&1; then
  if TNG_SECRET_KEY=local-compose-validation-secret TNG_API_TOKENS=local-compose-validation-token \
      QBITTORRENT_USERNAME=local-compose-validation-user \
      QBITTORRENT_PASSWORD=local-compose-validation-qb-password \
      TRANSMISSION_USERNAME=local-compose-validation-user \
      TRANSMISSION_PASSWORD=local-compose-validation-transmission-password \
      DELUGE_PASSWORD=local-compose-validation-deluge-password \
      docker compose --profile qbittorrent --profile transmission --profile deluge \
        -f "$ROOT/deploy/docker/compose.yml" \
        -f "$ROOT/deploy/docker/compose.qbittorrent.yml" \
        -f "$ROOT/deploy/docker/compose.transmission.yml" \
        -f "$ROOT/deploy/docker/compose.deluge.yml" config --format json \
        >"$TMP_DIR/sidecar-compose.json" 2>"$TMP_DIR/compose.log" &&
    jq -e '
      . as $cfg
      | ["torrentng", "torrentng-qbittorrent", "torrentng-transmission", "torrentng-deluge"] as $names
      | all($names[];
          . as $name
          | (($cfg.services[$name].cap_drop // []) | index("ALL")) != null
            and (($cfg.services[$name].security_opt // []) | index("no-new-privileges:true")) != null
            and any($cfg.services[$name].volumes[]?;
              .target == "/var/lib/torrentng" and .type == "volume")
        )
        and ([$names[] as $name
          | $cfg.services[$name].volumes[]?
          | select(.target == "/var/lib/torrentng")
          | .source] as $states
          | ($states | length) == 4 and ($states | unique | length) == 4)
        and any($cfg.services.torrentng.ports[]?;
          .target == 8080 and .host_ip == "127.0.0.1")
        and any($cfg.services.nginx.ports[]?;
          .target == 80 and .host_ip == "127.0.0.1")
        and all(["torrentng-qbittorrent", "torrentng-transmission", "torrentng-deluge"][];
          . as $name | any($cfg.services[$name].ports[]?;
            .target == 8080 and .host_ip == "127.0.0.1"))
        and all($names[];
          . as $name
          | (all(["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"][];
              . as $cap | (($cfg.services[$name].cap_add // []) | index($cap)) != null)
             and ($cfg.services[$name].user // null) == null))
        and any($cfg.services.qbittorrent.ports[]?;
          .target == 8080 and .host_ip == "127.0.0.1")
        and any($cfg.services.transmission.ports[]?;
          .target == 9091 and .host_ip == "127.0.0.1")
        and any($cfg.services.deluge.ports[]?;
          .target == 8112 and .host_ip == "127.0.0.1")
        and ($cfg.services.transmission.environment.USER == "local-compose-validation-user")
        and ($cfg.services.transmission.environment.PASS == "local-compose-validation-transmission-password")
        and ($cfg.services["torrentng-qbittorrent"].environment.TNG_QBITTORRENT_PASSWORD == "local-compose-validation-qb-password")
        and ($cfg.services["torrentng-deluge"].environment.TNG_DELUGE_PASSWORD == "local-compose-validation-deluge-password")
    ' "$TMP_DIR/sidecar-compose.json" >/dev/null &&
    docker compose -f "$ROOT/deploy/certification/compose.yml" config --format json \
      >"$TMP_DIR/certification-compose.json" 2>>"$TMP_DIR/compose.log" &&
    jq -e '
      . as $cfg
      | $cfg.services.torrentng as $service
      | (($service.cap_drop // []) | index("ALL")) != null
        and all(["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"][];
          . as $cap | (($service.cap_add // []) | index($cap)) != null)
        and ($service.user // null) == null
        and (($service.security_opt // []) | index("no-new-privileges:true")) != null
        and any($service.volumes[]?;
          .target == "/var/lib/torrentng" and .type == "volume")
        and any($service.ports[]?;
          .target == 8080 and .host_ip == "127.0.0.1")
        and all(["sonarr", "radarr", "prowlarr", "autobrr", "cross-seed"][];
          . as $name | any($cfg.services[$name].ports[]?; .host_ip == "127.0.0.1"))
    ' "$TMP_DIR/certification-compose.json" >/dev/null &&
    docker compose -f "$ROOT/deploy/docker/compose.phase1.yml" config --format json \
      >"$TMP_DIR/phase1-compose.json" 2>>"$TMP_DIR/compose.log" &&
    jq -e '
      .services["torrentng-phase1"] as $service
      | (($service.cap_drop // []) | index("ALL")) != null
        and all(["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"][];
          . as $cap | (($service.cap_add // []) | index($cap)) != null)
        and ($service.user // null) == null
        and (($service.security_opt // []) | index("no-new-privileges:true")) != null
        and any($service.ports[]?;
          .target == 8080 and .host_ip == "127.0.0.1")' \
      "$TMP_DIR/phase1-compose.json" >/dev/null &&
    PHASE1_INCOMING_PORT=51001 PHASE1_CONTAINER_INCOMING_PORT=52000 \
      docker compose -f "$ROOT/deploy/docker/compose.phase1.yml" config --format json \
      >"$TMP_DIR/phase1-custom-compose.json" 2>>"$TMP_DIR/compose.log" &&
    jq -e '
      .services["torrentng-phase1"] as $service
      | $service.environment.RTORRENT_INCOMING_PORT == "52000"
        and any($service.ports[]?;
          .target == 52000 and .published == "51001" and .protocol == "tcp")
        and any($service.ports[]?;
          .target == 52000 and .published == "51001" and .protocol == "udp")
    ' "$TMP_DIR/phase1-custom-compose.json" >/dev/null &&
    docker compose -f "$ROOT/deploy/interop/compose.yml" config --format json \
      >"$TMP_DIR/interop-compose.json" 2>>"$TMP_DIR/compose.log" &&
    jq -e '
      . as $cfg
      | [
        ($cfg.services.torrentngd.ports[] | select(.target == 8080)),
        ($cfg.services.qbittorrent.ports[] | select(.target == 8080)),
        ($cfg.services.transmission.ports[] | select(.target == 9091)),
        ($cfg.services.deluge.ports[] | select(.target == 8112)),
        ($cfg.services.deluge.ports[] | select(.target == 58846)),
        ($cfg.services.opentracker.ports[] | select(.target == 6969)),
        ($cfg.services["fixture-http"].ports[] | select(.target == 80))
      ] as $management_ports
      | ($management_ports | length) == 8
        and all($management_ports[]; .host_ip == "127.0.0.1")
        and (($cfg.services.torrentngd.cap_drop // []) | index("ALL")) != null
        and all(["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"][];
          . as $cap | (($cfg.services.torrentngd.cap_add // []) | index($cap)) != null)
        and ($cfg.services.torrentngd.user // null) == null
        and (($cfg.services.torrentngd.security_opt // []) | index("no-new-privileges:true")) != null
    ' "$TMP_DIR/interop-compose.json" >/dev/null &&
    TORRENTNG_API_TOKEN=local-native-compose-validation-token \
      docker compose --profile observability \
        -f "$ROOT/deploy/native/compose.yml" config --format json \
        >"$TMP_DIR/native-compose.json" 2>>"$TMP_DIR/compose.log" &&
      jq -e '
        . as $cfg
        | any($cfg.services.torrentngd.ports[]?;
          .target == 8080 and .host_ip == "127.0.0.1")
        and (($cfg.services.torrentngd.cap_drop // []) | index("ALL")) != null
        and all(["CHOWN", "DAC_OVERRIDE", "FOWNER", "SETGID", "SETUID"][];
          . as $cap | (($cfg.services.torrentngd.cap_add // []) | index($cap)) != null)
        and ($cfg.services.torrentngd.user // null) == null
        and (($cfg.services.torrentngd.security_opt // []) | index("no-new-privileges:true")) != null
        and any($cfg.services.prometheus.ports[]?;
          .target == 9090 and .host_ip == "127.0.0.1")
        and any($cfg.services.grafana.ports[]?;
          .target == 3000 and .host_ip == "127.0.0.1")
    ' "$TMP_DIR/native-compose.json" >/dev/null &&
    grep -Fq 'php/getsettings.php' "$ROOT/deploy/docker/compose.phase1.yml" &&
    grep -Fq 'php/getsettings.php' "$ROOT/scripts/phase1_certification.sh" &&
    grep -Fq 'INTEROP_QBITTORRENT_PASSWORD' "$ROOT/scripts/interop_matrix.sh" &&
    ! grep -Fq 'password=adminadmin' "$ROOT/scripts/interop_matrix.sh" &&
    ! grep -Eq 'seccomp([:=])unconfined' \
      "$ROOT/deploy/docker/compose.yml" \
      "$ROOT/deploy/docker/compose.phase1.yml" \
      "$ROOT/deploy/certification/compose.yml" \
      "$ROOT/deploy/systemd/rtorrentng-prod-readonly-datapool.conf" &&
    grep -Fq -- '--cap-drop=ALL' \
      "$ROOT/deploy/systemd/rtorrentng-prod-readonly-datapool.conf" &&
    grep -Fq -- '--security-opt no-new-privileges:true' \
      "$ROOT/deploy/systemd/rtorrentng-prod-readonly-datapool.conf" &&
    grep -Fq '"/var/lib/torrentng"' "$ROOT/deploy/docker/Dockerfile" &&
    grep -Fq 'USER root' "$ROOT/deploy/docker/Dockerfile" &&
    grep -Fq 'USER root' "$ROOT/deploy/docker/Dockerfile.phase1" &&
    grep -Fq 'deploy/container/identity.sh' "$ROOT/deploy/docker/Dockerfile.phase1" &&
    grep -Fq 'USER root' "$ROOT/deploy/native/Dockerfile" &&
    grep -Fq 'deploy/container/identity.sh' "$ROOT/deploy/docker/Dockerfile" &&
    grep -Fq 'deploy/container/identity.sh' "$ROOT/deploy/native/Dockerfile" &&
    grep -Fq 'TNG_UID must be a nonzero numeric ID' "$ROOT/deploy/native/Dockerfile" &&
    grep -Fq 'cap_drop:' "$ROOT/deploy/native/compose.yml" &&
    grep -Fq 'no-new-privileges:true' "$ROOT/deploy/native/compose.yml" &&
    grep -Fq 'cap_drop:' "$ROOT/deploy/interop/compose.yml" &&
    grep -Fq 'no-new-privileges:true' "$ROOT/deploy/interop/compose.yml" &&
    grep -Fq 'runAsNonRoot: true' "$ROOT/deploy/native/kubernetes/statefulset.yaml" &&
    grep -Fq 'automountServiceAccountToken: false' "$ROOT/deploy/native/kubernetes/statefulset.yaml" &&
    ! grep -Fq "user: \"\${PUID:-1000}:\${PGID:-1000}\"" "$ROOT/deploy/docker/compose.phase1.yml" &&
    grep -Fq './config:/config:ro' "$ROOT/deploy/docker/compose.yml" &&
    grep -Fq './config:/config:ro' "$ROOT/deploy/docker/compose.phase1.yml" &&
    grep -Fq 'tng_identity_enter' \
      "$ROOT/deploy/docker/entrypoint.sh" &&
    grep -Fq 'tng_identity_enter' \
      "$ROOT/deploy/docker/entrypoint.phase1.sh"; then
    mark "container deployment hardening" "PASS" "controlled runtime identity, read-only config binds, default seccomp, capability drops, durable sidecar state, loopback HTTP/API management ports, and required profile auth validated"
  else
    cat "$TMP_DIR/compose.log" >&2
    mark "container deployment hardening" "FAIL" "Compose security, non-root image, backend-auth, state-volume, PHP health, or loopback-port contract failed"
  fi
else
  mark "container deployment hardening" "BLOCKED" "Docker Compose plugin not installed"
fi

if command -v docker >/dev/null 2>&1; then
  if docker image inspect "$IMAGE" >/dev/null 2>&1; then
    mark "container image exists" "PASS" "$IMAGE"
  else
    mark "container image exists" "FAIL" "$IMAGE not found"
  fi

  if command -v trivy >/dev/null 2>&1; then
    if trivy image --quiet --severity HIGH,CRITICAL --exit-code 1 "$IMAGE" >"$TMP_DIR/trivy.txt" 2>&1; then
      mark "trivy image scan" "PASS" "no HIGH/CRITICAL findings"
    else
      mark "trivy image scan" "FAIL" "HIGH/CRITICAL findings"
    fi
  else
    if docker run --rm -v /var/run/docker.sock:/var/run/docker.sock aquasec/trivy:latest image --quiet --severity HIGH,CRITICAL --exit-code 1 "$IMAGE" >"$TMP_DIR/trivy.txt" 2>&1; then
      mark "trivy image scan" "PASS" "dockerized trivy found no HIGH/CRITICAL findings"
    else
      code=$?
      if grep -qi 'vulnerability' "$TMP_DIR/trivy.txt" 2>/dev/null; then
        mark "trivy image scan" "FAIL" "dockerized trivy found HIGH/CRITICAL findings"
      else
        mark "trivy image scan" "BLOCKED" "dockerized trivy failed with exit $code"
      fi
    fi
  fi

  if command -v trivy >/dev/null 2>&1; then
    if trivy config --severity HIGH,CRITICAL --exit-code 1 "$ROOT/deploy/docker" >"$TMP_DIR/trivy-config.txt" 2>&1; then
      mark "trivy config scan" "PASS" "no HIGH/CRITICAL deployment misconfigurations"
    else
      code=$?
      cat "$TMP_DIR/trivy-config.txt" >&2
      if grep -Eqi 'DS-[0-9]+ \((HIGH|CRITICAL)\)|HIGH: [1-9]|CRITICAL: [1-9]' "$TMP_DIR/trivy-config.txt"; then
        mark "trivy config scan" "FAIL" "HIGH/CRITICAL deployment misconfiguration found"
      else
        mark "trivy config scan" "BLOCKED" "Trivy config scan failed with exit $code"
      fi
    fi
  else
    if docker run --rm -v "$ROOT:/src:ro" -w /src aquasec/trivy:latest config --severity HIGH,CRITICAL --exit-code 1 deploy/docker >"$TMP_DIR/trivy-config.txt" 2>&1; then
      mark "trivy config scan" "PASS" "dockerized trivy found no HIGH/CRITICAL deployment misconfigurations"
    else
      code=$?
      cat "$TMP_DIR/trivy-config.txt" >&2
      if grep -Eqi 'DS-[0-9]+ \((HIGH|CRITICAL)\)|HIGH: [1-9]|CRITICAL: [1-9]' "$TMP_DIR/trivy-config.txt"; then
        mark "trivy config scan" "FAIL" "HIGH/CRITICAL deployment misconfiguration found"
      else
        mark "trivy config scan" "BLOCKED" "dockerized Trivy config scan failed with exit $code"
      fi
    fi
  fi
else
  mark "container image scan" "BLOCKED" "docker not installed"
  mark "trivy config scan" "BLOCKED" "docker not installed"
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
