#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${TNG_STORAGE_REPORT_DIR:-$ROOT/certification/reports}"
OUT="${TNG_STORAGE_REPORT_INDEX:-$REPORT_DIR/storage-certification-index.md}"

mkdir -p "$REPORT_DIR"

kind_for_report() {
  case "$(basename "$1")" in
    storage-hardware-*) printf 'hardware matrix' ;;
    storage-uring-graduation-*) printf 'io_uring capability/graduation' ;;
    storage-move-import-*) printf 'move/import' ;;
    *) printf 'storage' ;;
  esac
}

field_from_report() {
  local field="$1"
  local report="$2"
  sed -n "s/^- ${field}: //p" "$report" | head -1
}

uring_field_from_report() {
  local field="$1"
  local report="$2"
  sed -n "s/^- ${field}: //p" "$report" | tail -1
}

real_root_evidence_for_report() {
  local report="$1"
  case "$(basename "$report")" in
    storage-move-import-*)
      if grep -qE 'tng_storage_move_import .*root_confined=1' "$report"; then
        printf 'yes'
      else
        printf 'no'
      fi
      ;;
    *)
      printf 'n/a'
      ;;
  esac
}

selected_backend_for_report() {
  local report="$1"
  case "$(basename "$report")" in
    storage-uring-graduation-*) uring_field_from_report Selected "$report" ;;
    *) printf 'n/a' ;;
  esac
}

fixed_buffer_strategy_for_report() {
  local report="$1"
  case "$(basename "$report")" in
    storage-uring-graduation-*) uring_field_from_report 'Fixed-buffer strategy' "$report" ;;
    *) printf 'n/a' ;;
  esac
}

result_for_report() {
  local report="$1"
  if grep -qE 'Overall status: FAIL|Result: FAIL|\|[^|]+\| FAIL \|' "$report"; then
    printf 'FAIL'
  elif grep -qE 'Overall status: PASS|Result: PASS|\|[^|]+\| PASS \|' "$report"; then
    printf 'PASS'
  elif grep -qE '\|[^|]+\| SKIP' "$report"; then
    printf 'SKIP'
  else
    printf 'INFO'
  fi
}

{
  echo "# TorrentNG Storage Certification Index"
  echo
  echo "- Generated: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unknown)"
  echo "- Report directory: $REPORT_DIR"
  echo
  echo "| Report | Kind | Generated | Host | Commit | Target | Selected backend | Fixed-buffer strategy | Real-root evidence | Result |"
  echo "| --- | --- | --- | --- | --- | --- | --- | --- | --- | --- |"
  shopt -s nullglob
  reports=("$REPORT_DIR"/storage-hardware-*.md "$REPORT_DIR"/storage-uring-graduation-*.md "$REPORT_DIR"/storage-move-import-*.md)
  if [[ "${#reports[@]}" -eq 0 ]]; then
    echo "| _none_ | storage |  |  |  |  | INFO |"
  else
    for report in "${reports[@]}"; do
      rel="${report#$ROOT/}"
      generated="$(field_from_report Generated "$report")"
      host="$(field_from_report Host "$report")"
      commit="$(field_from_report Commit "$report")"
      target="$(field_from_report Target "$report")"
      if [[ -z "$target" ]]; then
        target="$(field_from_report 'Hardware root' "$report")"
      fi
      printf '| [%s](../../%s) | %s | %s | %s | %s | %s | %s | %s | %s | %s |\n' \
        "$(basename "$report")" \
        "$rel" \
        "$(kind_for_report "$report")" \
        "${generated:-unknown}" \
        "${host:-unknown}" \
        "${commit:-unknown}" \
        "${target:-multiple/see report}" \
        "$(selected_backend_for_report "$report")" \
        "$(fixed_buffer_strategy_for_report "$report")" \
        "$(real_root_evidence_for_report "$report")" \
        "$(result_for_report "$report")"
    done
  fi
} >"$OUT"

echo "storage certification index: $OUT"
