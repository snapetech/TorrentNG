#!/usr/bin/env python3
"""Validate and assemble curated TorrentNG release-note fragments."""

from __future__ import annotations

import argparse
import os
import re
import subprocess
import sys
from pathlib import Path
from typing import Any


RELEASE_NOTE_DIRECTORY = "release-notes"
CATEGORIES = {
    "added": "Added",
    "changed": "Changed",
    "fixed": "Fixed",
    "security": "Security",
    "removed": "Removed",
    "deprecated": "Deprecated",
}
AUDIENCES = {"users", "operators"}
FRONTMATTER_KEYS = {"category", "audience", "area", "action", "breaking"}


def git_output(*args: str) -> str:
    return subprocess.check_output(["git", *args], text=True)


def is_release_note_path(file_name: str) -> bool:
    path = Path(file_name)
    return (
        path.parent == Path(RELEASE_NOTE_DIRECTORY)
        and path.suffix == ".md"
        and path.name.lower() != "readme.md"
    )


def changed_release_note_files(base: str | None, head: str) -> list[dict[str, str]]:
    if base:
        output = git_output(
            "diff",
            "--name-status",
            "--no-renames",
            f"{base}...{head}",
            "--",
            RELEASE_NOTE_DIRECTORY,
        )
        entries = []
        for line in output.splitlines():
            columns = line.split("\t")
            if len(columns) == 2 and is_release_note_path(columns[1]):
                entries.append({"status": columns[0][0], "file": columns[1]})
        return entries

    paths = git_output("ls-tree", "-r", "--name-only", head, "--", RELEASE_NOTE_DIRECTORY)
    return [
        {"status": "A", "file": file_name}
        for file_name in paths.splitlines()
        if is_release_note_path(file_name)
    ]


def read_at_ref(ref: str, file_name: str) -> str:
    return git_output("show", f"{ref}:{file_name}")


def parse_release_note(file_name: str, content: str) -> dict[str, Any]:
    errors: list[str] = []
    normalized = content.replace("\r\n", "\n").replace("\r", "\n")
    match = re.fullmatch(r"---\n([\s\S]*?)\n---\n?([\s\S]*)", normalized)
    if not match:
        return {"file": file_name, "errors": ["must contain YAML frontmatter delimited by `---`"]}

    metadata: dict[str, str] = {}
    for line in match.group(1).split("\n"):
        if not line.strip():
            continue
        item = re.fullmatch(r"([a-z][a-z-]*):\s*(\S.*)", line)
        if not item:
            errors.append(f"has invalid frontmatter: {line}")
            continue
        key, value = item.groups()
        if key in metadata:
            errors.append(f'frontmatter key "{key}" appears more than once')
        metadata[key] = value.strip()

    for key in metadata:
        if key not in FRONTMATTER_KEYS:
            errors.append(f'frontmatter key "{key}" is not supported')

    category = metadata.get("category", "").lower()
    if category not in CATEGORIES:
        errors.append("category must be one of: " + ", ".join(CATEGORIES))

    audience = [
        part.strip().lower()
        for part in metadata.get("audience", "").split(",")
        if part.strip()
    ]
    if (
        not audience
        or any(part not in AUDIENCES for part in audience)
        or len(set(audience)) != len(audience)
    ):
        errors.append("audience must list users, operators, or both exactly once")

    area = metadata.get("area", "").lower()
    if not re.fullmatch(r"[a-z][a-z0-9-]{1,31}", area):
        errors.append("area must be a 2-32 character lowercase slug")

    action = re.sub(r"\s+", " ", metadata.get("action", "").strip())
    if not action:
        errors.append("action is required; use `none` when no action is needed")
    elif action.lower() != "none":
        if len(action) < 5 or len(action) > 200:
            errors.append("action must be 5-200 characters or exactly `none`")
        if re.search(r"<!--|-->|\b(?:todo|tbd|fill in)\b", action, re.I):
            errors.append("action contains a placeholder or HTML comment")

    breaking = metadata.get("breaking", "").lower()
    if breaking not in {"true", "false"}:
        errors.append("breaking must be either `true` or `false`")
    if breaking == "true" and action.lower() == "none":
        errors.append("breaking changes must describe an upgrade or operator action")

    body = re.sub(r"\s+", " ", match.group(2).strip())
    if not 30 <= len(body) <= 400:
        errors.append("body must be 30-400 characters and describe user impact")
    if re.search(r"<!--|-->|\b(?:todo|tbd|fill in)\b", body, re.I):
        errors.append("body contains a placeholder or HTML comment")
    if body and not re.match(r"[A-Z0-9`*_]", body):
        errors.append("body must start with a capitalized sentence")
    if body and not re.search(r"[.!?)]$", body):
        errors.append("body must end with sentence punctuation")

    return {
        "file": file_name,
        "category": category,
        "audience": audience,
        "area": area,
        "action": "none" if action.lower() == "none" else action,
        "breaking": breaking == "true",
        "body": body,
        "errors": errors,
    }


def read_release_notes(
    entries: list[dict[str, str]], head: str
) -> tuple[list[dict[str, Any]], list[str]]:
    notes: list[dict[str, Any]] = []
    errors: list[str] = []
    for entry in entries:
        if entry["status"] != "A":
            errors.append(f"{entry['file']}: fragments are append-only; add a new file")
            continue
        try:
            content = read_at_ref(head, entry["file"])
        except (OSError, subprocess.CalledProcessError) as error:
            errors.append(f"{entry['file']}: unable to read file ({error})")
            continue
        note = parse_release_note(entry["file"], content)
        errors.extend(f"{entry['file']}: {error}" for error in note["errors"])
        if not note["errors"]:
            notes.append(note)
    return notes, errors


def area_title(area: str) -> str:
    return " ".join(part.capitalize() for part in area.split("-"))


def format_curated_notes(notes: list[dict[str, Any]]) -> str:
    if not notes:
        return ""
    lines = ["### User-facing changes", ""]
    for category, title in CATEGORIES.items():
        category_notes = [note for note in notes if note["category"] == category]
        if not category_notes:
            continue
        lines.extend([f"#### {title}", ""])
        for note in category_notes:
            prefix = "**Breaking:** " if note["breaking"] else ""
            lines.append(f"- **{area_title(note['area'])}:** {prefix}{note['body']}")
            if note["action"] != "none":
                lines.append(f"  - **Action required:** {note['action']}")
        lines.append("")
    return "\n".join(lines).strip()


def has_explicit_no_release_note(body: str) -> bool:
    return bool(re.search(r"(?im)^\s*release-note\s*:\s*none\s*$", body))


def fail(errors: list[str]) -> int:
    if errors:
        print("Release-note validation failed:", file=sys.stderr)
        for error in errors:
            print(f"- {error}", file=sys.stderr)
        return 1
    return 0


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    subparsers = parser.add_subparsers(dest="command", required=True)

    check = subparsers.add_parser("check", help="validate a pull request's release note")
    check.add_argument("--base", required=True)
    check.add_argument("--head", required=True)
    check.add_argument("--body-env", default="PR_BODY")

    preview = subparsers.add_parser("preview", help="preview notes changed in a ref range")
    preview.add_argument("--base", required=True)
    preview.add_argument("--head", required=True)

    assemble = subparsers.add_parser("assemble", help="assemble notes for a release")
    assemble.add_argument("--base", default="")
    assemble.add_argument("--head", required=True)
    assemble.add_argument("--output", required=True)

    args = parser.parse_args()
    base = getattr(args, "base", None)
    entries = changed_release_note_files(base, args.head)
    notes, errors = read_release_notes(entries, args.head)

    if args.command == "check":
        body = os.environ.get(args.body_env, "")
        if not notes and not has_explicit_no_release_note(body):
            errors.append(
                "add one validated release-notes/*.md fragment, or include `release-note: none` "
                "on its own line in the pull request body or commit message for internal-only work"
            )
        if fail(errors):
            return 1
        print(format_curated_notes(notes) or "Internal-only change: no release note is required.")
        return 0

    if fail(errors):
        return 1

    body = format_curated_notes(notes)
    if not body:
        body = "### Release notes\n\nNo user-facing changes were recorded for this release. No action is required."

    if args.command == "preview":
        print(body)
        return 0

    Path(args.output).write_text(body.rstrip() + "\n", encoding="utf-8")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
