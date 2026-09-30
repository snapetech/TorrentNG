#!/usr/bin/env python3
"""Stage selected migration inputs without flattening or escaping the snapshot."""

from __future__ import annotations

import json
import os
import shutil
import stat
import sys
from pathlib import Path


def _relative_regular_file(root: Path, raw_path: object) -> tuple[Path, Path]:
    if not isinstance(raw_path, str) or not raw_path:
        raise ValueError("selected migration path is empty or not a string")

    candidate = Path(raw_path)
    if not candidate.is_absolute():
        candidate = root / candidate
    candidate = Path(os.path.abspath(candidate))
    try:
        relative = candidate.relative_to(root)
    except ValueError as error:
        raise ValueError(f"selected path escapes the rTorrent snapshot: {raw_path!r}") from error
    if not relative.parts:
        raise ValueError("selected migration path names the snapshot directory")

    current = root
    for part in relative.parts:
        current = current / part
        try:
            metadata = current.lstat()
        except OSError as error:
            raise ValueError(f"selected migration input is unavailable: {raw_path!r}") from error
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError(f"selected migration input traverses a symlink: {raw_path!r}")

    if not stat.S_ISREG(metadata.st_mode):
        raise ValueError(f"selected migration input is not a regular file: {raw_path!r}")
    return current, relative


def stage(report_path: Path, source_root: Path, destination_root: Path, manifest_path: Path) -> int:
    if source_root.is_symlink() or destination_root.is_symlink() or manifest_path.is_symlink():
        raise ValueError("snapshot, staging, and manifest paths cannot be symlinks")
    root = source_root.resolve(strict=True)
    destination = destination_root.resolve(strict=True)
    if not root.is_dir() or not destination.is_dir():
        raise ValueError("snapshot and staging paths must be directories")
    if any(destination.iterdir()):
        raise ValueError("staging directory must be empty")

    report = json.loads(report_path.read_text(encoding="utf-8"))
    torrents = report.get("torrents") if isinstance(report, dict) else None
    if not isinstance(torrents, list):
        raise ValueError("migration report has no torrent list")

    copied_by_relative: dict[Path, Path] = {}
    selected_sources: list[Path] = []
    for torrent in torrents:
        if not isinstance(torrent, dict):
            raise ValueError("migration report contains a malformed torrent entry")
        for field in ("torrent_path", "resume_path"):
            raw_path = torrent.get(field)
            if raw_path is None or raw_path == "":
                continue
            source, relative = _relative_regular_file(root, raw_path)
            previous = copied_by_relative.get(relative)
            if previous is not None:
                if previous != source:
                    raise ValueError(f"two migration inputs collide at {relative}")
                continue

            target = destination / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            current_parent = destination
            for part in relative.parts[:-1]:
                current_parent = current_parent / part
                if current_parent.is_symlink():
                    raise ValueError(f"staging path traverses a symlink: {relative}")
            shutil.copy2(source, target)
            copied_by_relative[relative] = source
            selected_sources.append(source)

    if not selected_sources:
        raise ValueError("migration report selected no input files")
    with manifest_path.open("wb") as manifest:
        for source in selected_sources:
            manifest.write(os.fsencode(source))
            manifest.write(b"\0")
    return len(torrents)


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 4:
        print(
            f"usage: {sys.argv[0]} REPORT_JSON SNAPSHOT_DIR STAGING_DIR SELECTED_PATHS_NUL",
            file=sys.stderr,
        )
        return 2
    try:
        count = stage(*(Path(value) for value in args))
    except (OSError, ValueError, json.JSONDecodeError) as error:
        print(f"migration staging refused: {error}", file=sys.stderr)
        return 2
    if count == 0:
        print("migration report selected no torrents", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
