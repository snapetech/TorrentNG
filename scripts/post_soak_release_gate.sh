#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/post-soak-release-$(date -u +%Y%m%dT%H%M%SZ).md}"

mkdir -p "$(dirname "$OUT")"

latest() {
  local pattern="$1"
  find "$REPORT_DIR" -maxdepth 1 -type f -name "$pattern" -printf '%T@ %p\n' 2>/dev/null \
    | sort -nr | awk 'NR==1 {print $2}'
}

overall() {
  local file="$1"
  if [[ -z "$file" || ! -f "$file" ]]; then
    printf 'MISSING'
    return
  fi
  awk -F': ' '
    /^Overall status:/ {status=$2}
    /test result: ok/ {ok=1}
    END {
      if (status) print status;
      else if (ok) print "PASS";
      else print "RUNNING/UNKNOWN";
    }
  ' "$file"
}

status="PASS"

mark() {
  local name="$1"
  local result="$2"
  local detail="$3"
  detail="${detail//$'\n'/ }"
  detail="${detail//|/\\|}"
  printf '| %s | %s | %s |\n' "$name" "$result" "$detail" >>"$OUT"
  case "$result" in
    PASS|INFO|WARN) ;;
    *) status="FAIL" ;;
  esac
}

gate() {
  local name="$1"
  local pattern="$2"
  local file result detail
  file="$(latest "$pattern")"
  result="$(overall "$file")"
  detail="$([[ -n "$file" ]] && basename "$file" || printf 'missing %s' "$pattern")"
  mark "$name" "$result" "$detail"
}

memory_gate() {
  local file result detail
  file="$(latest 'memory-roadmap-certification-*.md')"
  detail="$([[ -n "$file" ]] && basename "$file" || printf 'missing memory-roadmap-certification-*.md')"
  if [[ -z "$file" || ! -f "$file" ]]; then
    mark "memory roadmap certification" "MISSING" "$detail"
    return
  fi
  if grep -qE '\| [^|]+ \| FAIL \|' "$file"; then
    result="FAIL"
  elif grep -qE '\| [^|]+ \| WARN \|' "$file"; then
    result="WARN"
    detail="$detail; WARN rows are documented non-release claims"
  else
    result="PASS"
  fi
  mark "memory roadmap certification" "$result" "$detail"
}

storage_index_gate() {
  local file detail
  file="$(latest 'storage-certification-index.md')"
  detail="$([[ -n "$file" ]] && basename "$file" || printf 'missing storage-certification-index.md')"
  if [[ -z "$file" || ! -f "$file" ]]; then
    mark "storage certification index" "MISSING" "$detail"
    return
  fi
  if grep -qE '\| \[[^]]+\]\([^)]*\) \| [^|]+ \| [^|]+ \| [^|]+ \| [^|]+ \| [^|]+ \| PASS \|' "$file"; then
    mark "storage certification index" "PASS" "$detail; contains passing storage evidence"
  else
    mark "storage certification index" "FAIL" "$detail; no passing storage evidence rows found"
  fi
}

certification_status_gate() {
  local tmp result detail
  tmp="$(mktemp)"
  "$ROOT/scripts/certification_status.sh" "$REPORT_DIR" >"$tmp"
  if awk -F'|' '
      /^\| Post-soak release gate \|/ {next}
      /^\|/ && $3 ~ /FAIL|MISSING/ {bad=1}
      END {exit bad ? 0 : 1}
    ' "$tmp"; then
    result="FAIL"
    detail="$(awk -F'|' '
      /^\| Post-soak release gate \|/ {next}
      /^\|/ && $3 ~ /FAIL|MISSING/ {
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", $2);
        gsub(/^[[:space:]]+|[[:space:]]+$/, "", $3);
        print $2 "=" $3
      }
    ' "$tmp" | paste -sd ';' -)"
  else
    result="PASS"
    detail="all non-post-soak status rows are present and non-failing"
  fi
  rm -f "$tmp"
  mark "certification status rollup" "$result" "$detail"
}

{
  echo "# TorrentNG Post-Soak Release Gate"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- Report directory: $REPORT_DIR"
  echo
  echo "## Checks"
  echo
  echo "| Gate | Status | Evidence |"
  echo "|---|---|---|"
} >"$OUT"

gate "soak finalization" 'soak-final-*.md'
gate "local release gate" 'local-release-*.md'
memory_gate
storage_index_gate
certification_status_gate

{
  echo
  echo "## Boundaries"
  echo
  echo "- This gate rolls up the latest generated evidence. It does not replace a fresh real-device run on new release hardware."
  echo "- Memory roadmap WARN rows are allowed only when the warning is an explicit non-claim, such as final io_uring frame-pool slot graduation."
  echo
  echo "Overall status: $status"
} >>"$OUT"

echo "$OUT"
[[ "$status" == "PASS" ]]
