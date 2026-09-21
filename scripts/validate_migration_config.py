#!/usr/bin/env python3
"""Ensure migration rollback covers the exact configured TorrentNG state."""

from __future__ import annotations

import os
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError:  # Python 3.10 deployments may provide tomli.
    try:
        import tomli as tomllib  # type: ignore[no-redef]
    except ModuleNotFoundError:
        tomllib = None  # type: ignore[assignment,misc]


MAX_CONFIG_BYTES = 1024 * 1024


def _resolve_configured_path(raw: object, *, base: Path, label: str) -> Path:
    if not isinstance(raw, str) or not raw:
        raise ValueError(f"{label} must be a non-empty string")
    path = Path(raw)
    if not path.is_absolute():
        path = base / path
    return path.resolve(strict=False)


def validate(config_path: Path, requested_session_dir: Path) -> None:
    if tomllib is None:
        raise ValueError("Python tomllib (or tomli) is required to validate TorrentNG config")
    if config_path.stat().st_size > MAX_CONFIG_BYTES:
        raise ValueError("TorrentNG config exceeds the 1 MiB validation limit")

    config = tomllib.loads(config_path.read_text(encoding="utf-8"))
    daemon = config.get("daemon", {})
    db = config.get("db", {})
    if not isinstance(daemon, dict) or not isinstance(db, dict):
        raise ValueError("TorrentNG daemon/db config sections must be tables")

    raw_session = daemon.get("session_dir")
    if raw_session is None:
        raw_session = (
            str(Path(os.environ["HOME"]) / ".local/share/torrentngd")
            if os.environ.get("HOME")
            else "/var/lib/torrentngd"
        )
    session_dir = _resolve_configured_path(
        raw_session, base=Path.cwd(), label="daemon.session_dir"
    )
    requested = requested_session_dir.resolve(strict=True)
    if session_dir != requested:
        raise ValueError("--torrentngd-session-dir does not match daemon.session_dir in config")

    raw_database = db.get("path", "")
    if raw_database in (None, ""):
        database_path = session_dir / "state.db"
    else:
        # Config::db_path resolves custom relative paths under session_dir.
        database_path = _resolve_configured_path(
            raw_database, base=session_dir, label="db.path"
        )
    try:
        database_path.relative_to(session_dir)
    except ValueError as error:
        raise ValueError("db.path must remain inside daemon.session_dir for rollback safety") from error


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    if len(args) != 2:
        print(f"usage: {sys.argv[0]} CONFIG_TOML SESSION_DIR", file=sys.stderr)
        return 2
    try:
        validate(Path(args[0]), Path(args[1]))
    except (OSError, ValueError, UnicodeError) as error:
        print(f"migration config refused: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
