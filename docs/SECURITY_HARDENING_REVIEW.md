# Security hardening review

This document records the red-team and engineering-hardening issues found during the May 2026 ruthless review pass. It is intentionally blunt: if a behavior is compatibility-shaped, inert, or safe only behind localhost assumptions, that needs to be visible in code, docs, and release gates.

## Immediate fixes included in `hardening/ruthless-review-fixes`

- Workspace package metadata now declares `AGPL-3.0-or-later`, matching the README licensing posture.
- Explicit `TORRENTNGD_CONFIG` load failures now fail closed instead of silently falling back to default configuration.
- Configured native API tokens are passed into the qBittorrent facade state.
- Native API routes now use a route-level guard when API tokens are configured. `/health`, `/api/v1/auth/login`, and `/api/v1/auth/logout` remain public; operational reads such as `/metrics`, `/api/v1/logs`, `/api/v1/session-events`, `/api/v1/events`, `/api/v1/engine`, `/api/v1/storage`, and torrent listing/detail now require a valid bearer token or session cookie when tokens exist.
- qBittorrent compatibility routes now use a route-level guard when API tokens are configured. Bearer tokens or `SID` cookies matching a configured API token are accepted. If no API tokens are configured, legacy unauthenticated compatibility behavior remains available for localhost/dev deployments.

## Remaining P0/P1 hardening backlog

### P0: normalize auth across every facade

Native and qBittorrent are now guarded when API tokens are configured. Transmission, Deluge, and rTorrent compatibility surfaces still need the same treatment.

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

Several WebUI/facade stores are currently in memory: saved views, ratio groups, workflows, workflow runs, RSS rules, qBittorrent preferences/cookies/search/RSS state, and tag/category facade state.

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
- qBittorrent, Transmission, Deluge, rTorrent, and native REST endpoint/method probes;
- live add/list/mutate/remove flows;
- import/export corpus hashes;
- private-torrent DHT/PEX suppression evidence;
- storage-root escape tests;
- auth-on/auth-off facade behavior; and
- a machine-readable pass/fail matrix.
