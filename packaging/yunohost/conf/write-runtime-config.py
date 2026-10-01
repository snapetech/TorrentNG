#!/usr/bin/env python3
"""Set the WebUI's request prefix to the YunoHost mount path."""

import json
import os
from pathlib import Path
import sys


def main() -> None:
    if len(sys.argv) != 3:
        raise SystemExit("usage: write-runtime-config.py OUTPUT_PATH BASE_PATH")
    destination = Path(sys.argv[1])
    base_path = sys.argv[2].rstrip("/")
    if base_path == "/":
        base_path = ""
    destination.parent.mkdir(parents=True, exist_ok=True)
    temporary = destination.with_suffix(destination.suffix + ".tmp")
    temporary.write_text(
        "window.__TNG_BASE_PATH__ = " + json.dumps(base_path) + ";\n",
        encoding="utf-8",
    )
    os.replace(temporary, destination)


if __name__ == "__main__":
    main()
