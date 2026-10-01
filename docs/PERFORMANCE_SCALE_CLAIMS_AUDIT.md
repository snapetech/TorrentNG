# Performance, Disk, RAM, Swarm, and Count Claims Audit

Audit date: 2026-10-01  
Scope: repository copy, source paths, and retained reports. No tests or
benchmarks were run for this audit.  
Confidence: **high** on what each harness measures; **moderate** on how far a
single host or synthetic corpus generalizes; **unknown** for unmeasured
production-scale limits.

## Verdict

TorrentNG has real performance work and several useful measured outcomes. The
README is mostly careful: it says hardware and workload affect storage
performance, scopes interoperability, and does not promise 100,000-torrent
production capacity. The weak points are vague opening claims, old design goals
that read like measured performance, and two certification paths that can
report resource or hardware success without measuring the daemon or a physical
disk.

Do not describe the 512 MB resource-governor cap as a 512 MB process-RAM cap.
It limits selected leased buffers. Do not combine the 100,000-row API result,
the separate many-client API run, and the single-torrent soak into one
100,000-torrent concurrent-capacity claim.

## Claim-by-claim reading

| Area | What the checked-in code and reports show | What that supports |
|---|---|---|
| “High-volume efficiency” and “one fast interface” | The README opening gives no workload, comparison, latency, or backend scope. Benchmarks cover selected synthetic routes and one isolated peer-wire encoding loop. | Treat these as product intent. The code does not establish that all supported backends or workloads are fast, or that TorrentNG beats other clients. |
| 50,000-torrent API target | The sidecar benchmark seeds a synthetic SQLite corpus, requests the endpoint, and returns at most 5,000 rows. It records one elapsed value around `send()` before the response body is drained or parsed. The 2026-09-21 results are 33.72 ms for a 5,000-row page with 50,000 rows in the corpus. | A useful single-run service benchmark for a bounded page. It is not a 50,000-row response, a latency distribution, a TorrentNG Engine measurement, or end-to-end client download time. |
| 100,000-torrent native result | The 2026-09-02 release-binary run restored 100,000 SQLite rows, then exercised count, pagination, aggregate stats, restart, and one torrent promotion/demotion. 99,999 rows were synthetic stopped rows sharing one small metadata shape and save root; one valid torrent was seeded and promoted. The report records 120 MB RSS after restore, 33 ms for the first two-row page, and an explicit `PASS_WITH_LIMITATIONS`. | Strong evidence that the tested build can restore and serve a mostly dormant synthetic 100,000-row corpus. It is historical artifact evidence, not a current-build capacity guarantee, 100,000 active tasks, or a real metadata/peer/storage workload. |
| 32 JSON plus 8 slow SSE clients | The 2026-09-04 release binary served 204,936 requests in 30 seconds with zero errors and p99 10.08 ms. The canonical ledger says the corpus was small. The 8 SSE clients each received one event; this did not exercise sustained high-volume event fan-out. | Useful concurrent API responsiveness evidence for that binary and small-corpus mix. It does not combine with the separate 100,000-row run to prove concurrent 100,000-row operation. |
| 100,000 idle RAM check | `rt-metrics/tests/scale.rs` builds an in-process qBittorrent router with synthetic registry entries. It checks absolute test-process RSS below 2.5 GiB and limits growth to 64 FDs and 8 threads; it does not assert an RSS delta or exercise the native daemon restore path. | A coarse test-process proxy for that API object shape, not a fixed per-torrent RAM budget or engine RSS result. |
| 1,000 hot memory check | The test builds 1,000 synthetic `TorrentRuntimeStats` rows, then sums only the ten rows retained in `hot_torrent_memory_top` and checks that estimate is below 64 MiB. | A regression for top-ten attribution. It does not measure actual allocations, the governor total, or total memory across 1,000 hot torrents. |
| Process memory and FD/thread soak | The public 24-hour run completed 1,437 samples and retained the exact completed Debian torrent. However, `soak_certification.sh` reads `/proc/1/status` and `/proc/1/fd`. The native image entrypoint runs Tini as container PID 1, so the reported 1.3 MB RSS, 3 FDs, and 1 thread describe Tini rather than `torrentngd`. | The health, sync, metrics, completed-torrent, and disk-free observations remain useful. The run does not establish daemon RSS, daemon FD/thread ceilings, or a memory-qualified soak. |
| HDD peer-read elevator | The 2026-09-10 b393 report targets an ext4 filesystem on a rotational LVM device. Across three 128 MiB shuffled-read trials, the median was 5.11x wall-clock improvement; 8,192 peer reads were coalesced to one backend read. | A strong result for this code, host, filesystem, and access pattern. It is not a general torrent throughput claim or deterministic per-drive placement control. |
| `io_uring` storage | The 2026-09-10 HDD stream measured 209.79 MiB/s read for `pread` and 210.70 MiB/s for `io_uring`; throughput ratios were informational, not a required gate. A later report titled “hardware matrix” ran against tmpfs and inferred `Unknown`; its `io_uring` stream was only 4 MiB and sub-millisecond. | The HDD result shows comparable throughput on one host, not a material `io_uring` speedup. The later tmpfs PASS is capability/smoke evidence only, not hardware or disk-throughput evidence. |
| Public swarm | The public Debian v1 matrix at b393 completed a 791,674,880-byte torrent and recorded 142 Rust peers in a five-client matrix. The 24-hour run tracked one completed public torrent. | Real public-v1 transfer and continuity evidence. It does not qualify multiple swarms, a large public peer population over time, pure-v2 public interop, or 100,000 torrents. The default global peer connection limit is 200. |
| WebUI and torrent count | The browser scale test mocks 15,000 rows, checks fewer than 120 rendered rows, and exercises one “Load more” click. The live hook fetches 200 rows per page. | Good evidence for bounded DOM rendering with mocked data. Browsing all 100,000 rows would take 500 page advances; backend pagination does not make that manual interaction efficient. |
| Disk capacity | Storage APIs expose filesystem-reported total, used, and free bytes. The storage design document sets 200+ TB and “beat mainstream clients” as goals; it does not report a 200 TB TorrentNG run or a competitor comparison. | Report those as design targets only. Current real-root move/import fixtures support correctness at their tested fixture sizes, not multi-TB throughput. |

The direct qBittorrent facade also intentionally omits per-torrent transient
tracker, swarm, queue, and limit fields for responses over 200 torrents. That
keeps large reads from issuing one engine-actor round trip per torrent, but it
means large-page compatibility projections are not full live-swarm summaries.
The API documentation already states this tradeoff.

## What is strong enough to say

- TorrentNG implements bounded API pagination and snapshot cursors, tiered
  mostly-dormant runtime state, worker and queue limits, descriptor leases,
  and memory admission for selected buffer classes.
- A dated native release-binary run handled a synthetic 100,000-row mostly
  dormant corpus and exercised restart plus one-torrent promotion.
- A dated API/SSE run handled 204,936 requests from 32 JSON clients and 8 slow
  SSE clients with no request errors on its tested small corpus.
- A dated real-HDD test showed a 5.11x median speedup for one shuffled peer-read
  pattern, and a 32x reduction in backend reads for adjacent reads on the same
  target.
- A real public Debian v1 transfer completed and observed 142 TorrentNG peers
  in the five-client matrix.

Keep the build, host, fixture, response size, and date attached to each number.
These outcomes are valuable; their narrow scope is part of the claim.

## Rectification list

1. **Fix soak telemetry.** Select the `torrentngd` PID by executable identity
   or report validated container cgroup totals; record the image or binary
   digest and fail if the sampler resolves to Tini or another wrapper. Mark the
   historical RSS/FD/thread values invalid while retaining the service
   continuity result.
2. **Make hardware status mean hardware.** Distinguish local I/O smoke tests
   from physical-device qualification. Unknown, tmpfs, and network targets
   must not yield a hardware `PASS`; release hardware claims should identify
   the filesystem/device and apply the HDD performance gate when claiming HDD
   results. Keep `io_uring` selection separate from a throughput win.
3. **Scope performance language and benchmark records.** Replace unqualified
   “fast”/“high-volume” wording with the architecture and tested workload.
   Label the 50,000 benchmark as a 5,000-row response over a 50,000-row
   synthetic corpus; report full response-read timing and repeated percentiles
   for any user-facing latency claim. Include commit, artifact digest, CPU,
   device, filesystem, corpus shape, request concurrency, response rows, and
   whether measurements include body transfer.
4. **Correct RAM-proxy names and extend coverage only where useful.** Rename
   the 1,000-hot “memory cap” row to top-ten attribution unless it checks all
   1,000 rows. Give the 100,000 idle test an explicit RSS delta/per-row bound
   or call it a permissive test-process ceiling. Keep process RSS and
   governor-managed bytes as separate measurements.
5. **Separate count, swarm, and UI claims.** Keep 100,000 mostly-dormant count
   results separate from active-peer count and concurrent clients. Identify
   that the 142 peers came from a five-client public Debian matrix. Improve
   large-library WebUI navigation beyond one manual 200-row page at a time, or
   avoid implying that API pagination alone makes browsing all 100,000 rows
   responsive.
6. **Label the Storage NG document’s 200+ TB and competitor claims as goals.**
   The current title says “Design,” but the stated numbers and comparison
   should remain visibly prospective until a matching workload has been run.

The canonical tracked items for these fixes are TNG-145, TNG-146, and TNG-147
in [`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md).
