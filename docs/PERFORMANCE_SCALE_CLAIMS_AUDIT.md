# Performance, Disk, RAM, Swarm, and Count Claims Audit

Audit date: 2026-10-01  
Scope: repository copy, source paths, and retained reports. No tests or
benchmarks were run for this audit.  
Confidence: **high** on what each harness measures; **moderate** on how far a
single host or synthetic corpus generalizes; **unknown** for unmeasured
production-scale limits.

## Implementation update — 2026-10-02

The repository fixes the actionable findings from this audit: soak telemetry
now identifies `torrentngd` and records container/image IDs; the storage matrix
labels non-device targets as smoke-only and the release rollup requires a
qualified target; the WebUI supports direct row-range navigation; and public
documentation scopes capacity, memory, and comparative claims to their actual
evidence. A fresh local physical-device qualification has run, and a current-tree
idle-daemon soak is in progress; neither changes the scope of historical results.

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
| “RAM is O(active transfer)” | `STORAGE_NG.md` says the global frame pool makes RAM `O(active transfer)`. That statement describes frame-pool buffers only; registry rows, metadata, piece indexes, runtime tasks, database state, and other allocations also consume memory. | Say that frame-pool memory is capped and follows in-flight I/O. It does not describe the total process memory curve. |
| Process memory and FD/thread soak | The historical public 24-hour run completed 1,437 samples and retained the exact completed Debian torrent, but its sampler read `/proc/1` (Tini), not `torrentngd`. | Continuity, health, sync, metrics, completed-torrent, and disk-free observations remain useful. The run does not establish daemon resource ceilings; the current sampler fix requires a new artifact-bound run. |
| HDD peer-read elevator | The 2026-09-10 b393 report targets an ext4 filesystem on a rotational LVM device. Across three 128 MiB shuffled-read trials, the median was 5.11x wall-clock improvement; 8,192 peer reads were coalesced to one backend read. | A strong result for this code, host, filesystem, and access pattern. It is not a general torrent throughput claim or deterministic per-drive placement control. |
| `io_uring` storage | The 2026-09-10 HDD stream measured 209.79 MiB/s read for `pread` and 210.70 MiB/s for `io_uring`; throughput ratios were informational, not a required gate. A later report titled “hardware matrix” ran against tmpfs and inferred `Unknown`; its `io_uring` stream was only 4 MiB and sub-millisecond. | The HDD result shows comparable throughput on one host, not a material `io_uring` speedup. The later tmpfs PASS is capability/smoke evidence only, not hardware or disk-throughput evidence. |
| Public swarm | The public Debian v1 matrix at b393 completed a 791,674,880-byte torrent and recorded 142 Rust peers in a five-client matrix. The 24-hour run tracked one completed public torrent. | Real public-v1 transfer and continuity evidence. It does not qualify multiple swarms, a large public peer population over time, pure-v2 public interop, or 100,000 torrents. The default global peer connection limit is 200. |
| WebUI and torrent count | The browser scale test mocks 15,000 rows, checks fewer than 120 rendered rows, and exercises page loading. The live hook fetches 200 rows at a time; direct row-range navigation starts from a requested server-side offset. | Good evidence for bounded DOM rendering and direct navigation over mocked data. It does not establish real-library capacity or throughput. |
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

## Rectification status — 2026-10-02

1. **Soak telemetry — implemented; fresh soak PASS.** The sampler uses
   Docker's process table to identify the `torrentngd` executable and reads the
   host PID's resource counters, including for privilege-dropped containers.
   It rejects wrapper-only or ambiguous results and records container/image
   IDs. The current-tree 24-hour idle-daemon soak completed PASS; see the finalization report for sample-count and daemon RSS/FD/thread ceilings. Historical resource values remain invalid, and a public-torrent loaded soak is not covered.
2. **Hardware status — implementation and one fresh physical-device run PASS.** Unknown, pseudo-filesystem, and
   network targets are smoke-only. A release hardware PASS requires at least
   one eligible block-backed filesystem target. Reports record whether the
   HDD threshold was enforced and keep `io_uring` selection separate from a
   throughput win. The 2026-10-02 full storage release suite passes on
   `/dev/sdb`, Btrfs SSD; this does not add current HDD or multi-device evidence.
3. **Performance language — reconciled.** README claims now describe the
   supported interface and bounded architecture without unmeasured speed
   promises. Historical benchmark claims retain their fixture, response, host,
   artifact, and timing limits; no new latency claim was added.
4. **Memory wording — reconciled.** Frame-pool bounds are distinguished from
   process RSS and governor-managed allocations. The 1,000-hot result is
   described as top-ten attribution, not total memory across 1,000 torrents.
5. **Count, swarm, and UI claims — reconciled.** Synthetic mostly-dormant
   counts, public peers, concurrency, and mocked WebUI results remain separate.
   Direct row-range navigation is added and covered for offset/snapshot
   continuity; it does not establish real-library capacity.
6. **Storage design claims — reconciled.** `STORAGE_NG.md` labels 200+ TB and
   competitor comparisons as prospective targets until a matching workload
   has been measured.

The canonical tracked items for these fixes are TNG-145, TNG-146, and TNG-147
in [`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md).
