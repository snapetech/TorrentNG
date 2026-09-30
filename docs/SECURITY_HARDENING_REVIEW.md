# Security hardening review

> Historical baseline: this document preserves the original May 2026 red-team
> findings. The current disposition and verification record are maintained in
> [`BACKEND_AUDIT_BURN_DOWN.md`](BACKEND_AUDIT_BURN_DOWN.md), updated 2026-09-25.

This document records the red-team and engineering-hardening issues found during the May 2026 ruthless review pass. It is intentionally blunt: if a behavior is compatibility-shaped, inert, or safe only behind localhost assumptions, that needs to be visible in code, docs, and release gates.

## Current disposition (2026-09-25 UTC)

The historical checklist below is not an open work queue. The
repository-actionable hardening findings in the reviewed scope are implemented
in the current tree. The historical compatible-client container privilege
finding is fixed in the current images; deployment-specific ownership
migration for existing volumes and host mounts remains an operator task. The
linked local security review reports
are 2026-09-04 snapshots; the
newer egress coverage is recorded in the canonical backend audit. The local
reports ([native](../certification/reports/security-review-native-current-20260904.md),
[sidecar](../certification/reports/security-review-sidecar-current-20260904.md))
predate that additional coverage.

| Finding | Current state | Honest boundary |
|---|---|---|
| Facade authentication | The TorrentNG client, qBittorrent, Transmission, Deluge, and the rTorrent library entry point enforce the configured token boundary; sidecar session cookies are signed, HttpOnly, and Secure by default. | Reverse-proxy header handling, TLS termination, and deployed secret rotation require operator review. |
| Storage authority | Unix execute paths use configured/persisted server roots and descriptor-relative no-follow checks; Windows storage-plan operations use configured-root capabilities and handle-relative traversal, copy/verification/deletion, pruning, reconciliation, and no-replace rename. Caller roots are preview-only, and plans requiring publication rename fail closed when the target lacks atomic no-replace support. | Native Windows runtime behavior and hostile live filesystem races still require target-host qualification. |
| Metainfo integers and caps | Checked signed-to-unsigned conversions and parser limits reject negative, overflowing, and oversized torrent-controlled values. | Larger external corpus/fuzz runs remain qualification evidence. |
| Outbound egress | The shared address policy covers tracker/webseed requests, DHT targets/results, metadata-pending magnets, and v1/hybrid/pure-v2 peer connections; global peer bans also apply before metadata acquisition. Private, loopback, and link-local destinations are denied by default. | Public-network hostile DNS/redirect behavior and interoperability remain unverified; private-network operation requires explicit config opt-in. |
| Compatibility honesty | Unsupported queue/plugin behavior returns explicit unsupported results; pure-v2 metadata completion is implemented behind bounded BEP 9/BEP 52 validation, and projection-only state is documented. | Client-version breadth and public pure-v2 interoperability remain evidence questions. |
| Sidecar API identity and request bounds | Native torrent routes resolve canonical cache identities before backend calls; missing bulk IDs are reported; selections, delimited forms, nested mutation fan-out, peer-address lists, bulk-action concurrency, transient search state, category/tag field lengths, per-torrent tags, and aggregate tag assignments are bounded. Persistent category/tag dictionaries cap at 16,384 entries / 4 MiB; aggregate tag assignments cap at 1,000,000 links / 64 MiB, and list/delta projections fail closed if legacy data exceeds the cap. qBittorrent `hashes=all` is limited to 10,000 cache rows using a bounded lookahead query. | A concurrent external backend change can still race a capacity preflight and be rejected by the cache transaction; public-client interoperability remains unverified. |
| Tracker credential diagnostics | Sidecar tracker URLs in diagnostics are reduced to scheme/host/port; userinfo, paths, queries, fragments, magnets, and malformed values are masked. Mutation failure responses/logs do not echo backend error strings. | This covers TorrentNG-side diagnostics only; operator access to backend-native logs remains governed by each backend's logging policy. |
| Tracker-supplied warning/failure text | Native tracker warnings/failures and compatible-backend qBittorrent, Transmission, rTorrent, and TorrentNG status messages are treated as untrusted; URLs, magnets, path-like tokens with credential queries, common credential-like query/header fields (including multiply encoded names and values split across line breaks), terminal/bidi controls, and legacy native/sidecar SQLite values are sanitized before API projection. | Arbitrary non-URL secrets outside recognized credential fields are not exhaustively classified. |
| Workflow script output | Bounded stdout/stderr are escaped before logging; C0/C1 controls, Unicode line separators, and bidi controls cannot forge visible log records or terminal sequences. | Script output remains operational log content and can still contain arbitrary secrets printed by an explicitly configured script. |
| rTorrent log projection | Persisted rTorrent log lines reduce scheme-qualified URLs to origin, mask embedded magnets, redact common credential-like query/header fields, hide paths when a sensitive query is present, and escape C0/C1, Unicode line-separator, and bidi controls before returning operator events. | This classifies URLs and common credential fields, not every arbitrary secret format a backend might print in a non-URL log token. |
| Persisted operator events | Event messages and JSON string values are sanitized before storage and again when old `app_events` rows are projected to native and qBittorrent log APIs; sensitive-key values are replaced, and invalid legacy payload JSON fails closed. | Recognized URL/query/path forms are covered; path basenames and arbitrary unclassified non-URL secrets may remain. |
| Direct error tracing | Dynamic sidecar tracing error fields across API, qBittorrent, startup, stats, sync, and rTorrent log polling pass through shared URL/query/path/control redaction before emission. | Arbitrary non-URL secrets are not exhaustively classified; backend-native logs are outside this sidecar boundary. |
| Dynamic structured tracing fields | User-controlled category/tag/RSS labels, request method/path values, backend torrent identifiers, and configured data paths are sanitized before tracing emission; rTorrent source labels are sanitized at construction. | Recognized URL/path/credential patterns and controls are covered; new fields and arbitrary non-URL secrets still require review. |
| Sidecar SQLite mutex poisoning | Poisoned writer connections fail closed; the read pool skips poisoned slots and returns an error when all readers are poisoned. Poison conditions are logged once, and possibly interrupted SQLite connections are never resumed. | A poisoned writer remains unavailable for the process lifetime; restart/reopen recovery is not automatic. |
| Sidecar live-speed cache writes | Updates use a UUID-named sibling file opened with `create_new`, then atomically rename it into place; a pre-existing predictable `.tmp` symlink is not followed. | The configured parent directory remains operator-owned state and must not be writable by untrusted local processes. |
| rTorrent settings overlay persistence | The default Compose profile keeps `/config` read-only and writes UI-managed settings to the persistent state volume; the entrypoint creates and imports the configured overlay, new Unix overlay files are owner-only, and the UI checks actual directory writability. | A custom overlay path must be an operator-controlled writable persistent mount; the entrypoint appends its import at startup. |
| Native webseed and qBittorrent URL-fetch diagnostics | Native webseed URLs are reduced to scheme/host/port and reqwest failures discard the request URL; qBittorrent URL-fetch sources use origin-only labels and transport errors omit reqwest's URL. | Host/origin context remains visible. The qBittorrent failure regression is loopback-only; public-network behavior and broader compatibility remain unverified. |
| Async task panic diagnostics | Explicit Tokio task-join conversions in the native engine, storage jobs, compatibility projections, CLI workers, fastresume, upload-block reads, and sidecar blocking-task boundaries use static task/outcome summaries without retaining `JoinError` sources. The daemon installs an early panic hook that preserves source location but omits payload text from stderr. | The process hook applies to `torrentngd`; applications embedding the engine library own their hook policy. Sidecar synchronous worker panics use bounded fallbacks and safe diagnostics, not process-wide panic suppression. |
| Supervised task shutdown joins | Bounded shutdown joins inspect completed task failures as well as elapsed time across engine, DHT, storage-job, peer-listener, and session-event workers; post-abort cancellation is expected, panic and abort-grace expiry remain visible. uTP endpoint shutdown returns static panic/cancellation errors, and v2 peer cleanup logs panic outcomes. | Panic payloads are intentionally omitted; a timeout still forces abort and cannot recover a task that is permanently stuck in non-cancellable work. |
| Sidecar service-loop supervision | Sync, stats, and optional rTorrent log-ingestion handles are retained and monitored alongside HTTP serving; unexpected panic, cancellation, or normal return logs a safe summary and terminates the service instead of leaving stale projections online. Remaining loops are aborted on exit. | The service fails visibly rather than restarting a broken loop; a deployment process manager must restart it. |
| Configuration URL display | Workflow webhook and RSS feed rows show only validated HTTP(S) origins; tracker labels show only validated HTTP(S)/UDP origins. Userinfo, paths, query values, fragments, and malformed endpoint strings are not rendered by default. | The values remain available through the authenticated API and intentionally visible in the editor input or explicit tracker Reveal/Copy control; this is UI display protection, not API-side encryption. |
| Torrent ingestion failures | Magnet/URL/metainfo add failures and RSS-magnet results do not expose arbitrary backend error text that could echo tracker credentials; URL and magnet sources are redacted in sidecar logs. | Backend-native logs and the possibility of backend-side credential exposure are outside this boundary. |
| Durable operator state | TorrentNG-client state and sidecar qBittorrent RSS items/rules are persisted in their owning SQLite stores; qBittorrent search plugins/jobs remain process-local and are capped. | Search execution remains an inert compatibility surface; persistence does not imply a real search backend. |
| Metrics privacy and ingress | `/metrics` is auth-protected when tokens exist; hot-torrent labels hash identifiers by default, raw IDs are opt-in with a startup warning; peer ingress exports admission/rejection/read-error/timeout/malformed counters without peer labels; the native database worker exports queue, outcome, timeout, and cumulative latency/transaction metrics without operation, torrent, or peer labels. | Actual network exposure and reverse-proxy policy require deployment review; duration totals do not provide tail percentiles. |
| Compatible-client container confinement | Compatible-client and Phase 1 images run as configurable nonzero UID/GID, reject UID 0, mount `/config` read-only, and use `tini`; Compose drops all Linux capabilities, sets `no-new-privileges`, and retains Docker's default seccomp profile. Sidecar state uses per-service volumes. | Existing volumes need the documented one-time ownership migration. Host-mounted storage ownership and native target-LVM permissions remain unverified because production storage was not mounted for this pass. |
| Optional external backend profiles | Each backend now has a separate Compose overlay with required adapter credentials; backend WebUI/RPC host ports bind to loopback. Transmission credentials are wired to the image's `USER`/`PASS` variables. | qBittorrent and Deluge require one-time WebUI password configuration to match the adapter environment. Torrent peer ports remain externally reachable by design. |
| HTTP management-plane host ports | Compatible-client WebUI/API, its HTTP-only Nginx front door, optional adapter APIs, and native API Compose mappings bind to `127.0.0.1` by default; Compose contracts check those host bindings. | Remote access requires TLS termination and access control; BitTorrent peer ports remain intentionally published. |
| Phase 1 ruTorrent exposure | The unauthenticated ruTorrent UI binds to `127.0.0.1` by default; the Compose contract checks this. | Remote access requires an authenticated reverse proxy or equivalent access control before overriding `PHASE1_HTTP_BIND`. |
| Local certification/interop stacks | Their management, tracker, and fixture HTTP host ports bind to `127.0.0.1`; peer ports stay published for transfer tests. | These are disposable test stacks; do not expose their unauthenticated test services through a remote host binding. |
| Native observability profile | Prometheus and Grafana host ports bind to `127.0.0.1` by default. | Remote dashboard access requires an authenticated reverse proxy or explicit host access policy. |

Other remaining release gates are evidence-only: hosted CI observation, public
client/network traffic, real-device filesystem runs, and long soak. Do not copy
the historical requirements below into a new implementation plan without
first reconciling them against this section and the canonical backend ledger.

## Immediate fixes included in `hardening/ruthless-review-fixes`

- Workspace package metadata now declares `AGPL-3.0-or-later`, matching the README licensing posture.
- Explicit `TORRENTNGD_CONFIG` load failures now fail closed instead of silently falling back to default configuration.
- Configured TorrentNG API tokens are passed into the qBittorrent facade state.
- TorrentNG API routes now use a route-level guard when API tokens are configured. `/health`, `/api/v1/auth/login`, and `/api/v1/auth/logout` remain public; operational reads such as `/metrics`, `/api/v1/logs`, `/api/v1/session-events`, `/api/v1/events`, `/api/v1/engine`, `/api/v1/storage`, and torrent listing/detail now require a valid bearer token or session cookie when tokens exist.
- qBittorrent compatibility routes now use a route-level guard when API tokens are configured. Bearer tokens or `SID` cookies matching a configured API token are accepted. If no API tokens are configured, legacy unauthenticated compatibility behavior remains available for localhost/dev deployments.

## Historical P0/P1 hardening backlog

### P0: normalize auth across every facade

The TorrentNG client and qBittorrent surfaces are now guarded when API tokens are configured. Transmission, Deluge, and rTorrent compatibility surfaces still need the same treatment.

Required behavior:

- All compatibility facades must share one server-owned auth policy.
- Compatibility-login routes may emulate upstream clients, but must not mint unauthenticated sessions when tokens or passwords are configured.
- Insecure compatibility mode must be explicit, logged at startup, and visible from `/api/v1/engine`.
- The router layer, not individual handlers, should own default-deny behavior.

### P0: storage execution must not trust client-supplied roots

`/api/v1/storage/execute` currently accepts `roots` in the request. Root validation is only meaningful when roots come from server-owned configuration or persisted storage-root state, not from the caller.

Required behavior:

- Preview requests may include candidate roots for simulation.
- Execute requests must ignore caller-supplied roots and use configured storage roots only.
- Any execution attempt with no configured roots should fail closed.
- Storage-root authority should be tested with outside-root, symlink, broken symlink, and missing-ancestor cases.

### P0: metainfo numeric hardening

The metainfo parser currently reads several integer fields as `i64` and casts them to `u64`. Negative torrent lengths, file lengths, or piece lengths must never be allowed to wrap into huge unsigned values.

Required behavior:

- Replace direct `as u64` casts on torrent-controlled fields with checked helpers.
- Reject negative `length`, negative `piece length`, zero piece length, non-power-of-two piece length, and suspiciously huge values.
- Add caps for file count, path-component count, tracker count, webseed count, piece count, and total parsed metainfo size.
- Add tests for `-1`, `i64::MIN`, absurd piece counts, and huge tracker/webseed lists.

### P0: outbound URL policy

Tracker and webseed URLs are attacker-controlled inputs. They should be classified before any HTTP/UDP traffic is attempted.

Required behavior:

- Explicit egress policy for tracker/webseed schemes.
- Optional denylist for localhost, link-local, RFC1918/private ranges, metadata services, and `.local`/internal DNS targets.
- Clear private-torrent DHT/PEX/LSD suppression tests.
- Metrics and logs for rejected egress targets.

### P1: remove silent no-op compatibility semantics

Compatibility endpoints that accept a mutation but do not apply behavior should not return indistinguishable success.

Required behavior:

- Return structured `compat_noop`, `capability_unavailable`, or `accepted_inert` metadata where upstream-compatible status codes must remain successful.
- Log every inert compatibility mutation at warn/debug level with route, method, and reason.
- Expose backend capability detail in the WebUI so operators know what is real and what is façade-only.

### P1: persist operator-facing state

This historical finding included several in-memory WebUI/facade stores. qBittorrent RSS items and rules are now persisted in the sidecar SQLite cache; qBittorrent search plugins/jobs remain process-local and have explicit count and field limits. Treat the remaining stores according to their current source and ledger status rather than assuming they are still unresolved.

Required behavior:

- Classify state as `ephemeral`, `session`, or `persistent`.
- Persist operator-created workflows/rules/views unless explicitly marked temporary.
- Include persistence coverage in migration/export compatibility docs.

### P1: protect metrics from library fingerprinting

Metrics can reveal operational details and, in some cases, infohash labels. Even when auth is configured, high-cardinality torrent identifiers should be opt-in.

Required behavior:

- Keep `/metrics` protected when tokens are configured.
- Add `metrics.include_torrent_ids = false` default.
- Hash or suppress torrent labels unless explicitly enabled.
- Add a startup warning if identifiable metrics are enabled.

### P1: peer listener hostile-network hardening

The peer listener is a public network surface. It needs explicit DoS controls and tests, not just protocol parsing.

Required behavior:

- Global inbound accept semaphore.
- Handshake timeout.
- Per-IP throttling or penalty cache.
- Global cap for concurrent unauthenticated handshakes.
- Metrics for rejected, timed-out, malformed, and rate-limited peers.

## Release gate recommendation

Do not call universal compatibility release-ready until CI produces a downloadable compatibility report containing:

- exact client/container versions;
- qBittorrent, Transmission, Deluge, rTorrent, and TorrentNG REST endpoint/method probes;
- live add/list/mutate/remove flows;
- import/export corpus hashes;
- private-torrent DHT/PEX suppression evidence;
- storage-root escape tests;
- auth-on/auth-off facade behavior; and
- a machine-readable pass/fail matrix.
