#!/usr/bin/env python3
"""Validate credential-bearing backend URLs before making network requests.

Loopback HTTP targets are allowed for local certification. Remote targets must
use HTTPS and exactly match an origin listed in
TNG_PROTECTED_TARGET_ALLOWED_ORIGINS (comma-separated origins, without paths).
"""

from __future__ import annotations

import ipaddress
import os
import sys
from urllib.parse import urlsplit


ALLOWLIST_ENV = "TNG_PROTECTED_TARGET_ALLOWED_ORIGINS"
LEGACY_ALLOWLIST_ENV = "TNG_BACKEND_API_ALLOWED_ORIGINS"
PRIVATE_IPV4_NETWORKS = tuple(
    ipaddress.ip_network(network)
    for network in ("10.0.0.0/8", "172.16.0.0/12", "192.168.0.0/16")
)
PRIVATE_IPV6_ULA = ipaddress.ip_network("fc00::/7")


def _is_rfc1918_or_ula(address: ipaddress.IPv4Address | ipaddress.IPv6Address) -> bool:
    if isinstance(address, ipaddress.IPv6Address):
        mapped = address.ipv4_mapped
        if mapped is not None:
            address = mapped
        else:
            return address in PRIVATE_IPV6_ULA
    return any(address in network for network in PRIVATE_IPV4_NETWORKS)


def _origin(raw: str, *, origin_only: bool = False) -> tuple[str, bool, str]:
    if not raw or raw != raw.strip() or "\\" in raw:
        raise ValueError("URL is empty or contains whitespace/backslashes")
    if any(ord(character) <= 0x20 or ord(character) == 0x7F for character in raw):
        raise ValueError("URL contains a control or whitespace character")
    if "?" in raw or "#" in raw:
        raise ValueError("query strings and fragments are not allowed")

    try:
        parsed = urlsplit(raw)
        scheme = parsed.scheme.lower()
        hostname = parsed.hostname
        port = parsed.port
    except ValueError as error:
        raise ValueError("URL has an invalid host or port") from error

    if scheme not in {"http", "https"} or hostname is None:
        raise ValueError("URL must have an http or https scheme and a host")
    if parsed.username is not None or parsed.password is not None or "@" in parsed.netloc:
        raise ValueError("URL userinfo is not allowed")

    try:
        address = ipaddress.ip_address(hostname)
    except ValueError:
        if "%" in hostname:
            raise ValueError("scoped or escaped hostnames are not allowed")
        normalized_host = hostname.rstrip(".").encode("idna").decode("ascii").lower()
        if not normalized_host or len(normalized_host) > 253:
            raise ValueError("URL host is invalid")
        host_for_origin = normalized_host
        is_loopback = normalized_host == "localhost"
    else:
        normalized_host = address.compressed.lower()
        host_for_origin = f"[{normalized_host}]" if address.version == 6 else normalized_host
        is_loopback = address.is_loopback

    effective_port = port if port is not None else (443 if scheme == "https" else 80)
    if not 1 <= effective_port <= 65535:
        raise ValueError("URL port must be between 1 and 65535")
    canonical = f"{scheme}://{host_for_origin}:{effective_port}"

    if origin_only and parsed.path not in {"", "/"}:
        raise ValueError("allowlist entries must contain only an origin")
    if origin_only and parsed.query:
        raise ValueError("allowlist entries must not contain a query")
    if origin_only and parsed.fragment:
        raise ValueError("allowlist entries must not contain a fragment")
    return canonical, is_loopback, scheme


def validate_protected_target(
    url: str,
    allowed_origins: str | None = None,
    allow_private_http_origins: str | None = None,
) -> str:
    """Return a normalized base URL or raise ValueError if it is not trusted."""
    canonical, is_loopback, scheme = _origin(url)
    if allowed_origins is None:
        allowlist_text = os.environ.get(
            ALLOWLIST_ENV, os.environ.get(LEGACY_ALLOWLIST_ENV, "")
        )
    else:
        allowlist_text = allowed_origins
    allowed: set[str] = set()
    for item in allowlist_text.split(","):
        item = item.strip()
        if not item:
            continue
        allowed_origin, allowed_loopback, allowed_scheme = _origin(item, origin_only=True)
        if not allowed_loopback and allowed_scheme != "https":
            raise ValueError("remote allowlist origins must use HTTPS")
        allowed.add(allowed_origin)

    private_http_allowed: set[str] = set()
    if allow_private_http_origins:
        for item in allow_private_http_origins.split(","):
            item = item.strip()
            if not item:
                continue
            allowed_origin, allowed_loopback, allowed_scheme = _origin(item, origin_only=True)
            if allowed_scheme != "http" or allowed_loopback:
                raise ValueError("private HTTP exceptions must be non-loopback HTTP origins")
            private_http_allowed.add(allowed_origin)

    if is_loopback:
        return url.rstrip("/")
    if scheme != "https":
        if canonical in private_http_allowed:
            try:
                address = ipaddress.ip_address(urlsplit(url).hostname or "")
            except ValueError:
                address = None
            if address is not None and _is_rfc1918_or_ula(address):
                return url.rstrip("/")
        raise ValueError("remote credential-bearing targets must use HTTPS")
    if canonical not in allowed:
        raise ValueError(
            f"remote origin {canonical} is not listed in {ALLOWLIST_ENV}"
        )
    return url.rstrip("/")


def main(argv: list[str] | None = None) -> int:
    args = sys.argv[1:] if argv is None else argv
    allow_private_http_origins: str | None = None
    if args[:1] == ["--allow-private-http-origin"]:
        if len(args) != 3:
            print(
                f"usage: {sys.argv[0]} --allow-private-http-origin ORIGIN BASE_URL",
                file=sys.stderr,
            )
            return 2
        allow_private_http_origins, url = args[1], args[2]
    elif len(args) == 1:
        url = args[0]
    else:
        print(f"usage: {sys.argv[0]} BASE_URL", file=sys.stderr)
        return 2
    try:
        validate_protected_target(
            url, allow_private_http_origins=allow_private_http_origins
        )
    except ValueError as error:
        print(f"refusing protected target: {error}", file=sys.stderr)
        return 2
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
