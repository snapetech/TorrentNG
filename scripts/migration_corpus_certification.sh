#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
REPORT_DIR="${TNG_MIGRATION_CORPUS_REPORT_DIR:-$ROOT/certification/reports}"
OUT="${1:-$REPORT_DIR/migration-corpus-$(date -u +%Y%m%dT%H%M%SZ).md}"
CORPUS_DIR="${TNG_MIGRATION_CORPUS_DIR:-$ROOT/testdata/migration-corpus}"
REQUIRE_CORPUS="${TNG_REQUIRE_MIGRATION_CORPUS:-0}"
MANIFEST="$CORPUS_DIR/manifest.toml"

mkdir -p "$(dirname "$OUT")"

status="PASS"
missing=0
failed=0
manifest_status="SKIP"
manifest_detail="manifest.toml not present"
inventory="$OUT.inventory"
manifest_report="$OUT.manifest"

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

list_evidence() {
  local dir="$1"
  local expr=()
  local pattern

  if [[ ! -d "$dir" ]]; then
    return
  fi

  for pattern in "${evidence_patterns[@]}"; do
    if [[ "${#expr[@]}" -gt 0 ]]; then
      expr+=(-o)
    fi
    expr+=(-name "$pattern")
  done

  find "$dir" -type f \( "${expr[@]}" \) -print | sort
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
: > "$inventory"
: > "$manifest_report"

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
    while IFS= read -r evidence; do
      rel="${evidence#$ROOT/}"
      if command -v sha256sum >/dev/null 2>&1; then
        hash="$(sha256sum "$evidence" | awk '{print $1}')"
      else
        hash="sha256sum-unavailable"
      fi
      printf '| %s | %s | %s |\n' "$family" "$rel" "$hash" >> "$inventory"
    done < <(list_evidence "$dir")
  else
    row "$family" "MISSING" "0" "$dir"
    missing=$((missing + 1))
  fi
done

cat "$OUT.table" >> "$OUT"
rm -f "$OUT.table"

if [[ -f "$MANIFEST" ]]; then
  if python3 - "$MANIFEST" "$CORPUS_DIR" "$REQUIRE_CORPUS" "${families[@]}" >"$manifest_report" <<'PY'
import fnmatch
import hashlib
import pathlib
import sys
import tomllib

manifest = pathlib.Path(sys.argv[1]).resolve()
root = pathlib.Path(sys.argv[2]).resolve()
strict = sys.argv[3] == "1"
required = set(sys.argv[4:])
evidence_patterns = [
    "*.torrent",
    "*.fastresume",
    "*.resume",
    "*.resume.json",
    "*.state",
    "*.dat",
    "*.conf",
    "*.config",
    "resume.dat",
    "downloads.config",
    "torrents.config",
]

with manifest.open("rb") as fh:
    data = tomllib.load(fh)

families = data.get("family", [])
if not isinstance(families, list):
    raise SystemExit("manifest key [[family]] must be an array of tables")

seen = set()
artifact_rows = []
declared = set()
for family in families:
    if not isinstance(family, dict):
        raise SystemExit("each [[family]] entry must be a table")
    name = family.get("name")
    if not isinstance(name, str) or not name:
        raise SystemExit("each [[family]] entry needs a non-empty name")
    if name not in required:
        raise SystemExit(f"unknown source family in manifest: {name}")
    if name in seen:
        raise SystemExit(f"duplicate source family in manifest: {name}")
    seen.add(name)
    versions = family.get("versions", [])
    if not isinstance(versions, list) or not versions or not all(isinstance(v, str) and v for v in versions):
        raise SystemExit(f"{name}: versions must be a non-empty string array")
    expected = family.get("expected", [])
    if not isinstance(expected, list) or not expected or not all(isinstance(v, str) and v for v in expected):
        raise SystemExit(f"{name}: expected must be a non-empty string array")
    artifacts = family.get("artifacts", [])
    if artifacts and not isinstance(artifacts, list):
        raise SystemExit(f"{name}: artifacts must be an array of tables")
    if strict and not artifacts:
        raise SystemExit(f"{name}: strict corpus mode requires at least one declared artifact")
    for artifact in artifacts:
        if not isinstance(artifact, dict):
            raise SystemExit(f"{name}: each artifact must be a table")
        rel = artifact.get("path")
        source = artifact.get("source")
        permission = artifact.get("permission")
        if not isinstance(rel, str) or not rel:
            raise SystemExit(f"{name}: artifact path must be non-empty")
        path = (root / rel).resolve()
        if root not in path.parents and path != root:
            raise SystemExit(f"{name}: artifact path escapes corpus root: {rel}")
        try:
            rel_path = path.relative_to(root)
        except ValueError:
            raise SystemExit(f"{name}: artifact path escapes corpus root: {rel}")
        if not rel_path.parts or rel_path.parts[0] != name:
            raise SystemExit(f"{name}: artifact path must live under {name}/: {rel}")
        if not path.is_file():
            raise SystemExit(f"{name}: artifact path is missing: {rel}")
        if not isinstance(source, str) or not source:
            raise SystemExit(f"{name}: artifact {rel} needs source")
        if not isinstance(permission, str) or not permission:
            raise SystemExit(f"{name}: artifact {rel} needs permission")
        declared.add(rel_path.as_posix())
        expected_sha256 = artifact.get("sha256")
        actual_sha256 = hashlib.sha256(path.read_bytes()).hexdigest()
        if expected_sha256 is not None:
            if not isinstance(expected_sha256, str) or not expected_sha256:
                raise SystemExit(f"{name}: artifact {rel} sha256 must be a non-empty string")
            if expected_sha256.lower() != actual_sha256:
                raise SystemExit(
                    f"{name}: artifact {rel} sha256 mismatch: expected "
                    f"{expected_sha256.lower()} got {actual_sha256}"
                )
        artifact_rows.append((name, rel_path.as_posix(), source, permission, actual_sha256))

missing = sorted(required - seen)
if missing:
    raise SystemExit("manifest missing source families: " + ", ".join(missing))

if strict:
    discovered = set()
    for path in root.rglob("*"):
        if not path.is_file() or path.name == "manifest.toml":
            continue
        rel = path.relative_to(root).as_posix()
        if any(fnmatch.fnmatch(path.name, pattern) or fnmatch.fnmatch(rel, pattern) for pattern in evidence_patterns):
            discovered.add(rel)
    undeclared = sorted(discovered - declared)
    if undeclared:
        raise SystemExit("strict corpus manifest missing artifact declarations: " + ", ".join(undeclared))

print("| Source family | Artifact | Source | Permission | SHA-256 |")
print("|---|---|---|---|---|")
if artifact_rows:
    for row in artifact_rows:
        print("| " + " | ".join(cell.replace("|", "\\|") for cell in row) + " |")
else:
    print("| none declared | - | - | - | - |")
PY
  then
    manifest_status="PASS"
    manifest_detail="$MANIFEST"
  else
    manifest_status="FAIL"
    manifest_detail="$(tr '\n' ' ' <"$manifest_report")"
    status="FAIL"
  fi
fi

if [[ "$REQUIRE_CORPUS" == "1" && ! -f "$MANIFEST" ]]; then
  manifest_status="FAIL"
  manifest_detail="manifest.toml is required when TNG_REQUIRE_MIGRATION_CORPUS=1"
  status="FAIL"
fi

{
  echo
  echo "## Corpus Manifest"
  echo
  echo "- Manifest: $MANIFEST"
  echo "- Status: $manifest_status"
  echo "- Detail: $manifest_detail"
  echo
  if [[ "$manifest_status" == "PASS" ]]; then
    cat "$manifest_report"
  else
    echo 'Copy `manifest.example.toml` to `manifest.toml` and list every required source family.'
    echo 'Declared artifacts must stay under the matching source-family directory and include `path`, `source`, and `permission`.'
  fi
} >> "$OUT"
rm -f "$manifest_report"

{
  echo
  echo "## Evidence Inventory"
  echo
  echo "| Source family | File | SHA-256 |"
  echo "|---|---|---|"
  if [[ -s "$inventory" ]]; then
    cat "$inventory"
  else
    echo "| none | - | - |"
  fi
} >> "$OUT"
rm -f "$inventory"

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
  echo "When artifacts are present, copy manifest.example.toml to manifest.toml and record source/version/permission metadata."
  echo "Strict release mode requires a manifest, family-confined declared artifacts, and declarations for every discovered evidence file."
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
