#!/usr/bin/env python3
"""Write a private, correctly escaped TorrentNG TOML configuration."""

import json
import os
from pathlib import Path
import sys


def env(name: str, default: str = "") -> str:
    return os.environ.get(name, default)


def toml_string(value: str) -> str:
    return json.dumps(value, ensure_ascii=False)


def optional_string(name: str) -> str | None:
    value = env(name)
    return value if value else None


def native_config() -> str:
    data_dir = Path(env("TNG_DATA_DIR")) / "engine"
    download_dir = Path(env("TNG_DOWNLOAD_DIR"))
    return "\n".join(
        [
            "[daemon]",
            f'api_bind = {toml_string("127.0.0.1:" + env("TNG_HTTP_PORT"))}',
            f"session_dir = {toml_string(str(data_dir))}",
            "log_level = \"info\"",
            "",
            "[network]",
            f'listen_port = {int(env("TNG_PEER_PORT"))}',
            "max_peers = 200",
            "",
            "[storage]",
            f"download_dir = {toml_string(str(download_dir))}",
            "",
            "[db]",
            f"path = {toml_string(str(data_dir / 'state.db'))}",
            "",
            "[auth]",
            f"api_tokens = [{toml_string(env('TNG_API_TOKEN'))}]",
            "",
            "[logging]",
            'format = "json"',
            'profile = "basic"',
            'filter = ""',
            "event_retention = 10000",
            "",
        ]
    )


def existing_config() -> str:
    backend = env("TNG_BACKEND_TYPE")
    storage_root = env("TNG_STORAGE_ROOT")
    data_dir = Path(env("TNG_DATA_DIR")) / "sidecar"
    lines = [
        f'listen_addr = {toml_string("127.0.0.1:" + env("TNG_HTTP_PORT"))}',
        "sync_interval_secs = 5",
        f"data_dir = {toml_string(str(data_dir))}",
        f"storage_roots = [{toml_string(storage_root)}]" if storage_root else "storage_roots = []",
        "",
        "[backend]",
        f"type = {toml_string(backend)}",
    ]

    if backend == "rtorrent":
        transport = env("TNG_RTORRENT_TRANSPORT")
        lines.extend(["", "[rtorrent]"])
        if transport == "socket":
            lines.append(f"scgi_socket = {toml_string(env('TNG_RTORRENT_SOCKET'))}")
        else:
            lines.append(f"scgi_addr = {toml_string(env('TNG_RTORRENT_SCGI_ADDR'))}")
    elif backend == "qbittorrent":
        lines.extend(["", "[qbittorrent]", f"url = {toml_string(env('TNG_QBITTORRENT_URL'))}"])
        for key, value in (
            ("username", optional_string("TNG_QBITTORRENT_USERNAME")),
            ("password", optional_string("TNG_QBITTORRENT_PASSWORD")),
        ):
            if value is not None:
                lines.append(f"{key} = {toml_string(value)}")
    elif backend == "transmission":
        lines.extend(["", "[transmission]", f"url = {toml_string(env('TNG_TRANSMISSION_URL'))}"])
        for key, value in (
            ("username", optional_string("TNG_TRANSMISSION_USERNAME")),
            ("password", optional_string("TNG_TRANSMISSION_PASSWORD")),
        ):
            if value is not None:
                lines.append(f"{key} = {toml_string(value)}")
    elif backend == "deluge":
        lines.extend(
            [
                "",
                "[deluge]",
                f"url = {toml_string(env('TNG_DELUGE_URL'))}",
                f"password = {toml_string(env('TNG_DELUGE_PASSWORD'))}",
            ]
        )
    elif backend == "torrentng":
        lines.extend(
            [
                "",
                "[torrentng]",
                f"url = {toml_string(env('TNG_TORRENTNG_URL'))}",
                f"api_token = {toml_string(env('TNG_TORRENTNG_API_TOKEN'))}",
            ]
        )
    else:
        raise ValueError(f"unsupported backend type: {backend}")

    lines.extend(
        [
            "",
            "[auth]",
            f"secret_key = {toml_string(env('TNG_SESSION_SECRET'))}",
            f"api_tokens = [{toml_string(env('TNG_API_TOKEN'))}]",
            "trust_proxy_header = false",
            "",
        ]
    )
    return "\n".join(lines)


def main() -> None:
    if len(sys.argv) != 2:
        raise SystemExit("usage: write-config.py CONFIG_PATH")
    mode = env("TNG_DEPLOYMENT_MODE")
    if mode not in {"native", "existing"}:
        raise SystemExit("TNG_DEPLOYMENT_MODE must be native or existing")
    config = native_config() if mode == "native" else existing_config()
    destination = Path(sys.argv[1])
    destination.parent.mkdir(mode=0o750, parents=True, exist_ok=True)
    fd = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w", encoding="utf-8") as stream:
        stream.write(config)


if __name__ == "__main__":
    main()
