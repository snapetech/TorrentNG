#!/usr/bin/env python3
"""Concurrent native API/SSE load and slow-consumer evidence runner.

Only Python's standard library is required. The script deliberately reports
what it measures: request latency/bytes, concurrent SSE behavior, and an
optional daemon RSS sample. It is a repeatable evidence gate, not a synthetic
claim that a deployment can handle an arbitrary torrent count.
"""

from __future__ import annotations

import json
import os
import statistics
import sys
import threading
import time
from collections import defaultdict
from http.client import HTTPException
from typing import Any
from urllib.error import HTTPError, URLError
from urllib.request import Request, urlopen


ENDPOINTS = (
    "/health",
    "/api/v1/torrents?limit=200&offset=0",
    "/api/v1/transfer/info",
    "/api/v1/sidebar-facets",
    "/api/v1/logs?limit=50",
    "/api/v1/session-events?limit=50",
    "/api/qb/v2/torrents/info?limit=200",
    "/api/qb/v2/sync/maindata?rid=0",
)


def env_int(name: str, default: int, minimum: int = 1) -> int:
    try:
        return max(minimum, int(os.environ.get(name, str(default))))
    except ValueError:
        return default


def env_float(name: str, default: float, minimum: float = 0.1) -> float:
    try:
        return max(minimum, float(os.environ.get(name, str(default))))
    except ValueError:
        return default


class LoadStats:
    def __init__(self) -> None:
        self.lock = threading.Lock()
        self.requests: dict[str, dict[str, Any]] = defaultdict(
            lambda: {"count": 0, "success": 0, "errors": 0, "bytes": 0, "latencies_ms": [], "statuses": defaultdict(int)}
        )
        self.sse = {
            "started": 0,
            "completed": 0,
            "errors": 0,
            "bytes": 0,
            "events": 0,
            "lines": 0,
        }
        self.rss_samples: list[int] = []

    def request(self, endpoint: str, status: int, elapsed_ms: float, size: int) -> None:
        with self.lock:
            entry = self.requests[endpoint]
            entry["count"] += 1
            entry["bytes"] += size
            entry["latencies_ms"].append(elapsed_ms)
            entry["statuses"][str(status)] += 1
            if 200 <= status < 300:
                entry["success"] += 1
            else:
                entry["errors"] += 1

    def sse_update(self, key: str, amount: int = 1) -> None:
        with self.lock:
            self.sse[key] += amount

    def rss(self, value: int) -> None:
        with self.lock:
            self.rss_samples.append(value)


def request_once(base: str, token: str, endpoint: str, stats: LoadStats, timeout: float) -> None:
    headers = {"Accept": "application/json", "User-Agent": "torrentng-backend-load/1"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = Request(base + endpoint, headers=headers, method="GET")
    started = time.perf_counter()
    status = 599
    size = 0
    try:
        with urlopen(request, timeout=timeout) as response:
            status = int(response.status)
            body = response.read(8 * 1024 * 1024)
            size = len(body)
    except HTTPError as error:
        status = int(error.code)
        try:
            size = len(error.read(64 * 1024))
        except (OSError, HTTPException):
            size = 0
    except (OSError, HTTPException, URLError):
        status = 599
    elapsed_ms = (time.perf_counter() - started) * 1000.0
    stats.request(endpoint, status, elapsed_ms, size)


def api_client(base: str, token: str, deadline: float, stats: LoadStats, client_id: int) -> None:
    index = client_id % len(ENDPOINTS)
    while time.monotonic() < deadline:
        request_once(base, token, ENDPOINTS[index % len(ENDPOINTS)], stats, 5.0)
        index += 1


def slow_sse_client(base: str, token: str, deadline: float, delay: float, stats: LoadStats) -> None:
    headers = {"Accept": "text/event-stream", "Cache-Control": "no-cache", "User-Agent": "torrentng-backend-load/1"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = Request(base + "/api/v1/events?batch_size=1", headers=headers, method="GET")
    stats.sse_update("started")
    try:
        with urlopen(request, timeout=5.0) as response:
            line = bytearray()
            while time.monotonic() < deadline:
                chunk = response.read(1)
                if not chunk:
                    break
                stats.sse_update("bytes", len(chunk))
                line.extend(chunk)
                if chunk == b"\n":
                    stats.sse_update("lines")
                    if b"event: torrent_delta" in line:
                        stats.sse_update("events")
                    line.clear()
                time.sleep(delay)
            stats.sse_update("completed")
    except (OSError, HTTPException, HTTPError, URLError):
        stats.sse_update("errors")


def rss_bytes(pid: str) -> int | None:
    if not pid.isdigit():
        return None
    try:
        with open(f"/proc/{pid}/status", encoding="utf-8") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    fields = line.split()
                    return int(fields[1]) * 1024
    except (FileNotFoundError, PermissionError, ValueError):
        return None
    return None


def rss_sampler(pid: str, deadline: float, stats: LoadStats) -> None:
    while time.monotonic() < deadline:
        value = rss_bytes(pid)
        if value is not None:
            stats.rss(value)
        time.sleep(0.25)


def quantile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = min(len(ordered) - 1, int(round((len(ordered) - 1) * fraction)))
    return ordered[index]


def metrics_snapshot(base: str, token: str) -> tuple[int, dict[str, str]]:
    headers = {"Accept": "text/plain"}
    if token:
        headers["Authorization"] = f"Bearer {token}"
    request = Request(base + "/metrics", headers=headers, method="GET")
    with urlopen(request, timeout=5.0) as response:
        body = response.read(8 * 1024 * 1024)
    names = (
        "torrentng_api_sse_clients",
        "torrentng_api_sse_events_total",
        "torrentng_api_sse_lagged_total",
        "torrentng_api_sse_disconnects_total",
        "torrentng_api_sse_resyncs_total",
        "torrentng_api_response_bytes_estimated_total",
        "torrentng_api_snapshot_refreshes_total",
        "torrentng_api_snapshot_incremental_updates_total",
    )
    values: dict[str, str] = {}
    text = body.decode("utf-8", errors="replace")
    for line in text.splitlines():
        for name in names:
            if line.startswith(name + " "):
                values[name] = line.split()[-1]
    return len(body), values


def write_report(
    report: str,
    raw: str,
    base: str,
    duration: float,
    clients: int,
    sse_clients: int,
    slow_delay_ms: int,
    stats: LoadStats,
    metrics_size: int | None,
    metrics: dict[str, str],
    pid: str,
    started_at: float,
    finished_at: float,
    preflight_error: str | None = None,
) -> bool:
    request_rows: list[dict[str, Any]] = []
    all_latencies: list[float] = []
    total_requests = total_success = total_errors = total_bytes = 0
    for endpoint, entry in sorted(stats.requests.items()):
        latencies = list(entry["latencies_ms"])
        all_latencies.extend(latencies)
        total_requests += entry["count"]
        total_success += entry["success"]
        total_errors += entry["errors"]
        total_bytes += entry["bytes"]
        request_rows.append(
            {
                "endpoint": endpoint,
                "count": entry["count"],
                "success": entry["success"],
                "errors": entry["errors"],
                "bytes": entry["bytes"],
                "p50_ms": quantile(latencies, 0.50),
                "p95_ms": quantile(latencies, 0.95),
                "p99_ms": quantile(latencies, 0.99),
                "statuses": dict(entry["statuses"]),
            }
        )
    rss = list(stats.rss_samples)
    sse = dict(stats.sse)
    overall_ok = preflight_error is None and total_requests > 0 and total_errors == 0 and sse["started"] == sse_clients and sse["errors"] == 0
    if preflight_error is not None:
        overall = "NOT_RUN"
    elif overall_ok:
        overall = "PASS"
    else:
        overall = "FAIL"
    payload = {
        "base_url": base,
        "duration_seconds": duration,
        "clients": clients,
        "sse_clients": sse_clients,
        "slow_delay_ms": slow_delay_ms,
        "started_epoch": started_at,
        "finished_epoch": finished_at,
        "total_requests": total_requests,
        "total_success": total_success,
        "total_errors": total_errors,
        "total_response_bytes": total_bytes,
        "overall_p50_ms": quantile(all_latencies, 0.50),
        "overall_p95_ms": quantile(all_latencies, 0.95),
        "overall_p99_ms": quantile(all_latencies, 0.99),
        "requests": request_rows,
        "sse": sse,
        "rss_bytes": {"samples": len(rss), "min": min(rss) if rss else None, "max": max(rss) if rss else None, "delta": (rss[-1] - rss[0]) if len(rss) >= 2 else None},
        "metrics_response_bytes": metrics_size,
        "metrics": metrics,
        "preflight_error": preflight_error,
        "overall_status": overall,
    }
    with open(raw, "w", encoding="utf-8") as handle:
        json.dump(payload, handle, indent=2, sort_keys=True)
        handle.write("\n")
    with open(report, "w", encoding="utf-8") as handle:
        handle.write("# TorrentNG Native API Load Evidence\n\n")
        handle.write(f"- Date UTC: {time.strftime('%Y-%m-%dT%H:%M:%SZ', time.gmtime())}\n")
        handle.write(f"- Base URL: {base}\n- Duration: {duration:.1f}s\n- JSON clients: {clients}\n")
        handle.write(f"- Slow SSE clients: {sse_clients}\n- Slow SSE delay: {slow_delay_ms}ms per byte\n")
        handle.write(f"- Raw evidence: `{os.path.basename(raw)}`\n\n")
        handle.write("This is production-process HTTP evidence for bounded list/snapshot, aggregate, log, and SSE paths. RSS is a process-level allocation proxy only; no allocator profile is claimed.\n\n")
        if preflight_error:
            handle.write(f"Preflight: **NOT RUN** — {preflight_error}\n\nOverall status: NOT_RUN\n")
            return False
        handle.write("## Summary\n\n")
        handle.write("| Measure | Value |\n|---|---:|\n")
        handle.write(f"| Requests | {total_requests} |\n| Successful responses | {total_success} |\n| Errors/timeouts | {total_errors} |\n| Response bytes | {total_bytes} |\n")
        handle.write(f"| Overall p50 | {quantile(all_latencies, 0.50) or 0:.2f} ms |\n| Overall p95 | {quantile(all_latencies, 0.95) or 0:.2f} ms |\n| Overall p99 | {quantile(all_latencies, 0.99) or 0:.2f} ms |\n")
        handle.write(f"| SSE started/completed/errors | {sse['started']}/{sse['completed']}/{sse['errors']} |\n| SSE bytes/events/lines | {sse['bytes']}/{sse['events']}/{sse['lines']} |\n")
        if rss:
            handle.write(f"| RSS samples/min/max/delta | {len(rss)}/{min(rss)}/{max(rss)}/{rss[-1] - rss[0]} bytes |\n")
        else:
            handle.write("| RSS allocation proxy | NOT MEASURED (set TNG_LOAD_PID) |\n")
        handle.write("\n## Endpoint latency and allocation proxy\n\n| Endpoint | Requests | Errors | Bytes | p50 | p95 | p99 |\n|---|---:|---:|---:|---:|---:|---:|\n")
        for row in request_rows:
            handle.write(f"| `{row['endpoint']}` | {row['count']} | {row['errors']} | {row['bytes']} | {row['p50_ms'] or 0:.2f} ms | {row['p95_ms'] or 0:.2f} ms | {row['p99_ms'] or 0:.2f} ms |\n")
        handle.write("\n## Server metrics after load\n\n")
        if metrics:
            for name, value in sorted(metrics.items()):
                handle.write(f"- `{name}`: {value}\n")
        else:
            handle.write("- unavailable\n")
        handle.write(f"\nOverall status: {overall}\n")
    return overall_ok


def main() -> int:
    report = sys.argv[1] if len(sys.argv) > 1 else os.environ.get("TNG_LOAD_REPORT", "backend-api-load.md")
    raw = os.environ.get("TNG_LOAD_RAW", report.removesuffix(".md") + ".json")
    base = os.environ.get("TNG_BASE_URL", "http://127.0.0.1:28080").rstrip("/")
    token = os.environ.get("TNG_API_TOKEN", os.environ.get("TNG_RELEASE_TOKEN", ""))
    duration = env_float("TNG_LOAD_DURATION_SECONDS", 30.0)
    clients = env_int("TNG_LOAD_CLIENTS", 32)
    sse_clients = env_int("TNG_LOAD_SSE_CLIENTS", 8, 0)
    slow_delay_ms = env_int("TNG_LOAD_SLOW_DELAY_MS", 250, 0)
    pid = os.environ.get("TNG_LOAD_PID", "")

    started = time.time()
    preflight_error: str | None = None
    try:
        request = Request(base + "/health", headers={"Authorization": f"Bearer {token}"} if token else {})
        with urlopen(request, timeout=5.0) as response:
            if response.status != 200:
                preflight_error = f"health returned HTTP {response.status}"
            else:
                body = json.loads(response.read(1024 * 1024).decode("utf-8"))
                if body.get("ready") is not True:
                    preflight_error = "health returned ready=false"
    except (OSError, HTTPException, HTTPError, URLError, ValueError) as error:
        preflight_error = f"health unavailable: {error}"

    stats = LoadStats()
    metrics_size: int | None = None
    metrics: dict[str, str] = {}
    if preflight_error is None:
        deadline = time.monotonic() + duration
        threads = [threading.Thread(target=api_client, args=(base, token, deadline, stats, index), daemon=True) for index in range(clients)]
        threads.extend(threading.Thread(target=slow_sse_client, args=(base, token, deadline, slow_delay_ms / 1000.0, stats), daemon=True) for _ in range(sse_clients))
        if pid:
            threads.append(threading.Thread(target=rss_sampler, args=(pid, deadline, stats), daemon=True))
        for thread in threads:
            thread.start()
        for thread in threads:
            thread.join()
        try:
            metrics_size, metrics = metrics_snapshot(base, token)
        except (OSError, HTTPException, HTTPError, URLError):
            metrics_size = None
    finished = time.time()
    ok = write_report(report, raw, base, duration, clients, sse_clients, slow_delay_ms, stats, metrics_size, metrics, pid, started, finished, preflight_error)
    print(report)
    return 0 if ok else 1


if __name__ == "__main__":
    raise SystemExit(main())
