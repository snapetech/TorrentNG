#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${TNG_MIGRATION_CORPUS_REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/migration-corpus-$(date -u +%Y%m%dT%H%M%SZ).md}"
CORPUS_DIR="${TNG_MIGRATION_CORPUS_DIR:-$ROOT/testdata/migration-corpus}"
REQUIRE_CORPUS="${TNG_REQUIRE_MIGRATION_CORPUS:-0}"

mkdir -p "$(dirname "$OUT")"

status="PASS"
missing=0
failed=0

families=(
  qbittorrent
  transmission
  deluge
  utorrent
  biglybt
  tixati
  rtorrent
  generic
)

evidence_patterns=(
  "*.torrent"
  "*.fastresume"
  "*.resume"
  "*.resume.json"
  "*.state"
  "*.dat"
  "*.conf"
  "*.config"
  "resume.dat"
  "downloads.config"
  "torrents.config"
)

count_evidence() {
  local dir="$1"
  local expr=()
  local pattern

  if [[ ! -d "$dir" ]]; then
    printf '0'
    return
  fi

  for pattern in "${evidence_patterns[@]}"; do
    if [[ "${#expr[@]}" -gt 0 ]]; then
      expr+=(-o)
    fi
    expr+=(-name "$pattern")
  done

  find "$dir" -type f \( "${expr[@]}" \) | wc -l | tr -d ' '
}

row() {
  local family="$1"
  local result="$2"
  local files="$3"
  local evidence="$4"
  printf '| %s | %s | %s | %s |\n' "$family" "$result" "$files" "$evidence" >> "$OUT.table"
}

{
  echo "# TorrentNG Migration Corpus Certification"
  echo
  echo "- Date UTC: $(date -u +%Y-%m-%dT%H:%M:%SZ)"
  echo "- Host: $(hostname)"
  echo "- Commit: $(git -C "$ROOT" rev-parse --short HEAD 2>/dev/null || echo unavailable)"
  echo "- Corpus directory: $CORPUS_DIR"
  echo "- Strict corpus required: $REQUIRE_CORPUS"
  echo
  echo "## Synthetic Import/Apply Baseline"
  echo
  echo '```text'
} > "$OUT"
: > "$OUT.table"

if (cd "$ROOT" && cargo test -p rt-migrate) >> "$OUT" 2>&1; then
  echo '```' >> "$OUT"
else
  echo '```' >> "$OUT"
  status="FAIL"
  failed=1
fi

{
  echo
  echo "## Exported Corpus Coverage"
  echo
  echo "| Source family | Result | Evidence files | Evidence root |"
  echo "|---|---|---:|---|"
} >> "$OUT"

for family in "${families[@]}"; do
  dir="$CORPUS_DIR/$family"
  files="$(count_evidence "$dir")"
  if [[ "$files" -gt 0 ]]; then
    row "$family" "PASS" "$files" "$dir"
  else
    row "$family" "MISSING" "0" "$dir"
    missing=$((missing + 1))
  fi
done

cat "$OUT.table" >> "$OUT"
rm -f "$OUT.table"

{
  echo
  echo "## Required Layout"
  echo
  echo '```text'
  echo "testdata/migration-corpus/qbittorrent/"
  echo "testdata/migration-corpus/transmission/"
  echo "testdata/migration-corpus/deluge/"
  echo "testdata/migration-corpus/utorrent/"
  echo "testdata/migration-corpus/biglybt/"
  echo "testdata/migration-corpus/tixati/"
  echo "testdata/migration-corpus/rtorrent/"
  echo "testdata/migration-corpus/generic/"
  echo '```'
  echo
  echo "Place real exported client resume/config/torrent artifacts under each source family."
  echo "Set TNG_REQUIRE_MIGRATION_CORPUS=1 to make missing source-family corpora fail this gate."
  echo
  echo "- Missing source families: $missing"
} >> "$OUT"

if [[ "$failed" -ne 0 ]]; then
  status="FAIL"
elif [[ "$missing" -gt 0 && "$REQUIRE_CORPUS" == "1" ]]; then
  status="FAIL"
elif [[ "$missing" -gt 0 ]]; then
  status="PASS_WITH_GAPS"
fi

{
  echo
  echo "Overall status: $status"
} >> "$OUT"

echo "$OUT"
[[ "$status" != "FAIL" ]]
