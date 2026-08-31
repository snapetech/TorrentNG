# WebUI Live UX/Engineering Audit

Status as of 2026-08-29: **all findings below have been fixed in source**
(`main`, uncommitted at the time of writing). The live instance at the time
of the original audit (2026-08-28) was running an older deployed build than
`main` — several things flagged here as broken were already fixed in source
and just hadn't been rebuilt/redeployed yet; those are called out explicitly.
**None of these fixes have been deployed to production** — that's a separate,
explicit step (rebuild the sidecar image, redeploy to `kspls0`) that wasn't
taken as part of this pass. See "Fix pass" below for what changed and how it
was verified.

## Scope and methodology

Headless, authenticated, read-only inspection of the live production
deployment (`rtorrentng-prod`, rTorrent-backed Track 1 sidecar) via
Playwright/Chromium, driven programmatically against the real WebUI over
HTTPS through the production reverse proxy. No synthetic/staging data — the
library under test holds 6,956 real torrents (~1.2 TB downloaded, ~488 GB
uploaded, 290 TB pool at 97.8% used) across 25 trackers and 19 categories.

No destructive actions were taken: no torrents were added, deleted, renamed,
moved, or rechecked, and no settings were changed. Findings below come from
DOM inspection, network/console monitoring, `axe-core` automated
accessibility scans, and viewport emulation (desktop 1600×1000, tablet
810×1080, mobile 390×844).

Severity is ranked by user/operator impact, not implementation cost.

**A note on sensitive data**: the live instance renders multiple private
tracker passkeys in cleartext (see P0-3). This document deliberately does
**not** reproduce any of them — findings reference the exposure pattern only.

## Executive summary — top issues

| # | Severity | Area | Finding |
| --- | --- | --- | --- |
| P0-1 | Critical | Mobile/responsive | The WebUI has no responsive layout. On a phone-width viewport the filter sidebar and status-bar footer alone consume the entire screen, leaving ~2 visible torrent rows. |
| P0-2 | Critical | Real-time architecture | No WebSocket connection is ever opened. The client instead full-refetches the visible 200-row page and `/transfer/info` every 2 seconds, indefinitely, contradicting the project's own "delta sync via WebSocket, no full-refresh polling loops" requirement. |
| P0-3 | Critical | Security/privacy | Tracker announce URLs with embedded passkeys are shown in cleartext, unmasked, in at least three places (torrent detail panel, Properties modal, Settings → Library → Tracker Health) — the latter lists **all 25 trackers'** full passkeys on one page. |
| P0-4 | Critical | Data integrity | The sidebar "TYPE" filter (inferred media type) is effectively broken: filtering by **TV** returns 6,546 of 6,956 torrents (94% of the library), including plain-text ebooks and FLAC audio. Filtering by **Games** matches on loose name substrings ("Empire *Games*", an ebook) rather than actual content type. |
| P1-1 | High | Reliability | Operator logs show the rTorrent backend sync (`d.multicall.range`) failing and self-recovering repeatedly — 15+ times in the ~35-hour log window sampled, roughly hourly. |
| P1-2 | High | Compatibility | The Backend diagnostics page itself reports `rpc.trusted_connection_accept_all.set = no` — i.e., the exact rTorrent 0.16.9+ untrusted-XMLRPC problem this project exists to solve (per `docs/AUDIT.md`) is flagged unresolved on this production instance. |
| P1-3 | High | Scale/pagination | Torrent list pagination is a manual "Load more" button (~1s per 200-row batch), not infinite scroll or true virtualization over the full server-side set. At the project's stated 100k-torrent target this is ~500 manual clicks to reach the end of the list. |
| P1-4 | High | Data fidelity | Large, long-seeded torrents on ratio-strict private trackers (HDBits, TorrentLeech) consistently show **Ratio 0.00**, suggesting upload-byte history was not preserved across a migration/import — a real risk of ratio bans on those trackers. |
| P2-1 | Medium | Accessibility | `axe-core` found a serious color-contrast failure (4.39:1 vs. required 4.5:1) on toolbar action links/buttons in the detail panel, plus sitewide moderate landmark/heading violations. |
| P2-2 | Medium | Filtering | Sidebar TYPE facet counts do not recompute against an active search query (they stay at sitewide totals) while the "All types" count does — inconsistent faceting. |
| P2-3 | Medium | IA | Settings → Support bundles thousands of raw operator log lines, cosmetic theme/appearance settings, media-type-inference config, and community links onto a single page. |
| P2-4 | Medium | Findability | Tracker Health reports 208 active tracker errors, but there is no filter/view anywhere in the main Torrents list to isolate which torrents are affected. |
| P3 | Low/polish | Various | See "Polish issues" below (encoding bug, stale per-file %, always-on "Unsaved view" badge, missing form attributes, etc). |

## What's working well

Worth stating plainly, since the rest of this document is almost entirely
findings: the core is solid.

- **Performance**: page load ~1s to network-idle, search round-trip ~250ms,
  server TTFB ~5ms on a 6,956-torrent library — meets the 10k-torrent
  benchmark targets in `CLAUDE.md` today.
- **Detail panel and Properties modal** (General/Trackers/Files/Limits tabs)
  are well organized and cover the fields operators actually need.
- **Right-click context menu** is complete: Properties, Edit, Stop, Recheck,
  Reannounce, sequential toggle, copy hash/name, delete.
- **Column customization**: 21 selectable columns, reset/compact presets.
- **21 built-in visual themes** plus a working light/dark toggle (verified —
  initially assumed dark-only from the theme names, which was wrong).
- **Backend diagnostics page** (Settings → Backend) exposing live
  capability/method availability against the actual rTorrent build is a
  genuinely excellent transparency feature, rare in this category of
  software — it's also what surfaced P1-2.
- **Add-torrent dialog** correctly disables submit until a file/magnet/URL is
  staged; clean drag-drop affordance.
- Free-text **search matching is accurate** — it's specifically the inferred
  TYPE facet that's broken, not search itself.
- **Category-based filtering** (explicit, user/import-assigned) sums and
  filters correctly (category facet counts summed to 6,937 + 19 uncategorized
  ≈ 6,956 total) — a useful contrast with the broken inferred-TYPE facet.

## Critical findings (detail)

### P0-1: No responsive/mobile layout

At a 390×844 viewport (iPhone-class), the desktop layout is not adapted:

- The top nav bar wraps into 4 stacked rows before any content is visible
  (no hamburger/overflow menu).
- The full desktop STATE filter sidebar renders in place, unshortened.
- The status-bar footer (12+ stat chips: connection, DL/UL rates, totals,
  disk, conn/FW/DHT/PEX, donation links) wraps into a large block that
  competes with the sidebar for the same limited vertical space.
- Net result: **roughly 2 torrent rows are visible** in the primary content
  area on first load of a 6,956-torrent library.
- Column headers beyond NAME/STATUS are pushed off-screen with no visible
  scroll affordance.
- Tapping a row *does* work and opens a well-adapted, full-width stacked
  detail view — so the detail panel has responsive treatment; the list/shell
  chrome does not.
- At tablet width (810px) the layout is the unmodified desktop grid: the
  theme selector and the Dark/Light toggle button are clipped at the
  viewport's right edge.

This is the single highest-impact usability finding in this audit — a user
checking their seedbox from a phone cannot meaningfully use the torrent list
today.

### P0-2: No WebSocket; full-list polling every 2 seconds

`CLAUDE.md` states as a webui constraint: *"Delta sync via WebSocket — no
full-refresh polling loops."* Live network capture over multiple 8–15 second
windows shows:

- `GET /api/v1/transfer/info` every ~2s.
- `GET /api/v1/torrents?sort=name&dir=asc&offset=0&limit=200` every ~2s, in
  lockstep with the above — i.e., the entire current 200-row page is
  refetched from scratch every 2 seconds regardless of whether anything
  changed.
- `page.on('websocket')` never fires across any tested view — **no WS
  connection is opened at any point**, despite `sidecar::api::ws` existing
  in the codebase per `CLAUDE.md`.
- `GET /api/v1/events` is polled every ~7s and **returns HTTP 404 on every
  single call**, both logged-out and logged-in, spamming the browser console
  indefinitely with no backoff or circuit-breaker.

At 6,956 torrents this already means the client issues ~4 API requests every
2 seconds indefinitely, per open tab. At the project's 10k–100k target, or
with multiple simultaneous browser tabs/users, this polling pattern will not
scale and directly contradicts the architecture the project committed to.

### P0-3: Tracker passkeys exposed in cleartext, unmasked

Full tracker announce URLs — including the passkey/API-key query parameter
that functions as a bearer credential on private trackers — are rendered as
plain, un-obfuscated text with no reveal-on-demand or masking:

- Torrent detail side panel → TRACKERS section.
- Properties modal → Trackers tab and General tab (TRACKER field).
- **Settings → Library → Tracker Health** — this page lists all 25
  configured trackers with their full announce URLs (including passkeys) in
  one continuous, easily-screenshotted list.

Most modern clients (qBittorrent, Deluge) either mask the passkey by default
or treat the announce URL as sensitive. Here it's plain text in a read-only
`<input>`/text node with a "Copy" affordance right next to it, on a page a
user is likely to open (and possibly screen-share or screenshot, as this
audit itself demonstrates) when troubleshooting tracker connectivity. A
leaked passkey generally requires a tracker support ticket to rotate and can
otherwise be used to impersonate the account.

**Recommendation**: mask passkeys by default (e.g. `...announce.php?passkey=••••••••`)
with an explicit "reveal" click/copy action, at minimum on the Tracker Health
overview page where many trackers are listed at once.

### P0-4: Inferred media-type classification is unreliable

The sidebar "TYPE" facet (`inferred`) is one of the primary navigation/filter
tools for a large library, but produces results a user would immediately
distrust:

- Searching "grisham" (John Grisham ebooks) returns 4 correct hits. Applying
  the **TV** type filter on top of that search still shows the same 4 ebook
  results — the filter chip is visibly active (`Type tv ×`) but has no
  effect on the result set.
- Clearing search and filtering by **TV** alone returns 6,546 of 6,956
  torrents (94%), including obvious non-TV items: `.epub` ebooks, FLAC
  albums, and audiobook rips.
- Filtering by **Games** (40 results) returns items like
  `Charles Stross - Empire Games 02...` (an ebook) — matched on the
  substring "Games" in the filename rather than actual file type.
- Summing all TYPE bucket counts (Ebooks 3,688 + TV 6,546 + Video 2,378 +
  Audio 1,019 + ISO 4 + Games 40 + Software 67 ≈ 13,742) is roughly **double**
  the actual library size of 6,956 — torrents are being counted into
  multiple, often-wrong buckets.
- Root cause, confirmed via Settings → Support → "Media type inference": the
  default mode is **Full** — *"Use suffixes plus name, category, tag, and
  path hints"* — a loose heuristic. **Suffix only** and **Hints only** modes
  exist but were not the default and were not evaluated for accuracy in this
  audit (changing the setting was avoided to not alter behavior for other
  users/sessions, though the panel notes this control is browser-local).

By contrast, explicit user/import-assigned **categories** filter and count
correctly — this is specifically an inferred-classification problem, not a
general filtering bug.

**Recommendation**: re-evaluate the default inference mode's accuracy
against a real mixed library (this one is a good test corpus), or make the
TYPE facet visibly "best-effort" (e.g., muted styling, tooltip disclaimer)
rather than presenting it with the same visual weight as exact-match STATE
and CATEGORY filters.

## High-severity findings (detail)

### P1-1: Backend sync instability (from the app's own operator logs)

Settings → Support → Operator Logs shows a recurring pattern across the
sampled ~35-hour window:

```
WARN  rtorrent_sync_error      backend sync failed  error=d.multicall.range main offset=<N> limit=100
INFO  rtorrent_sync_recovered  backend sync recovered
```

This pair recurs 15+ times in the visible log window (roughly hourly, at
varying offsets into the 6,956-torrent multicall range), each time
self-recovering within seconds to ~20 seconds. It is not fatal today, but it
is frequent, unexplained in the UI (no operator-facing alert beyond the raw
log line), and is exactly the kind of failure mode that would compound with
scale (larger multicall ranges, more frequent chunking) as the library grows
toward the project's 10k–100k target.

### P1-2: RPC trusted-connection capability flagged unresolved

Settings → Backend → Capabilities shows:

```
RPC trusted connection toggle    no      rpc.trusted_connection_accept_all.set
```

highlighted in red, alongside a "4 drift" / "14/15 XMLRPC" badge summary.
Per `docs/AUDIT.md`, this exact capability gap is the root cause of
`load.start` and related calls being rejected for untrusted XMLRPC callers —
the headline problem Track 1 was built to fix for Prowlarr/Sonarr/Radarr/
autobrr integrations. Seeing it still flagged `no` on a production instance
that has Prowlarr, Sonarr, Radarr, autobrr, and cross-seed all deployed
alongside it is worth root-causing: either the mitigation isn't applied here,
or the diagnostic itself needs to account for whatever mitigation *is* in
place (e.g. if `RTORRENT_SCGI_SOCKET` trusted-path access makes this
particular flag moot, the diagnostics page should say so rather than show a
bare red "no").

### P1-3: Pagination doesn't scale to the stated target

- The list loads 200 rows initially; reaching more requires clicking
  "Load more torrents" (confirmed: scrolling the last rendered row into view
  does **not** trigger further loading — no infinite-scroll behavior).
- Each click takes ~1s and loads exactly 200 more rows.
- On this 6,956-torrent library, reaching the end requires ~35 manual
  clicks. At the project's 100k-torrent target, that's ~500 clicks.
- This does not by itself violate the "must handle 100k rows" virtualization
  requirement (rendering is presumably fine once loaded — only ~200 DOM rows
  exist at a time per the earlier "Rendered 200" status chip), but the
  *acquisition* of the full dataset for filtering/sorting/browsing purposes
  is manual and does not scale.

**Recommendation**: infinite scroll (or a much larger fetch window) driven
by scroll position, keeping virtualized rendering as-is.

### P1-4: Ratio 0.00 on large, long-seeded private-tracker torrents

Sampling large torrents added years ago on ratio-enforcing private trackers
(HDBits, TorrentLeech) — e.g., a 32.8 GB item added 2014, several 100+ GB
items added 2019–2023 — consistently shows **Ratio 0.00 / Uploaded 0 KB**
despite active "Seeding" status and years of elapsed time. This is
consistent with upload-byte accounting not surviving a client migration or
import (see `torrentngd migrate`/`rt-migrate` fidelity-bucket framework in
`CLAUDE.md`). This is a UI observation, not a confirmed root-cause diagnosis
— it wasn't possible to inspect the underlying DB/session state from the
browser — but it's worth an explicit fidelity check against
`crates/rt-migrate/tests/round_trip_matrix.rs` for upload-byte/ratio
preservation specifically, given the real risk (ratio bans) if it's real.

## Medium-severity findings (detail)

### P2-1: Accessibility (axe-core automated scan)

Ran against Torrents list, torrent-selected detail panel, and Settings:

| Impact | Rule | Where | Detail |
| --- | --- | --- | --- |
| Serious | `color-contrast` | Detail panel | "Announce", "Properties", "Edit selected" toolbar labels and the "Reannounce" button render at 4.39:1 (accent `#4f8cff` on `#202b42`) — just under the 4.5:1 AA threshold for normal text. |
| Moderate | `region` | All views | 7 nodes of page content sit outside any landmark region. |
| Moderate | `page-has-heading-one` | All views | No level-one heading (`<h1>`) present anywhere in the app shell. |
| Moderate | `landmark-unique` | Detail panel | A landmark role/label collides with another on the page. |

None of these are blocking, but they're cheap, concrete fixes (a CSS
token-value nudge, one `<h1>` in the shell, wrapping the three-pane layout in
`<main>`/`<nav>`/`<aside>` landmarks).

Separately (not axe-flagged, but observed): the login form's two `<input>`
elements have **no `name`, `id`, or `autocomplete` attributes**, and the
password field's `<label>` isn't programmatically associated via `for`. This
breaks password-manager autofill/save prompts and screen-reader label
announcement on the very first screen every user sees.

### P2-2: Sidebar facets don't recompute against active search

With a search query active, the "All types" count under TYPE correctly
reflects the filtered result count, but the individual type buckets
(Ebooks, TV, Video, …) continue to show sitewide totals rather than
recomputing against the search. This makes the facet counts actively
misleading while a search is in progress (e.g., "TV: 6,546" is displayed
next to a 4-result ebook search).

### P2-3: Settings information architecture

The **Support** tab (Settings → Support) contains, on one continuously
scrolling page: thousands of raw `INFO`/`WARN` operator log entries (a
diagnostics/ops concern), **Appearance** (theme palette + dark/light —  a
cosmetic browser preference), **Media type inference** (a data/classification
setting directly responsible for P0-4), and Discord/GitHub/donation links (a
support/community concern). These are four different audiences and mental
models sharing one page. A user trying to fix the P0-4 classification issue
has to know to look under "Support," not "Library" or "Backend," to find the
control that affects it.

**Recommendation**: move raw Operator Logs to their own tab (or under
Backend, next to the sync-error evidence in P1-1); keep Support to
links/community; consider whether Media type inference belongs under
Library (where the TYPE facet itself lives) instead.

### P2-4: No way to isolate torrents with tracker errors

Settings → Library → Tracker Health reports **208 errors** across trackers,
and the sidebar visually flags ~11 of 25 trackers with a "!" icon. But:

- The STATE sidebar filter's "Errored" bucket shows **0** — a different,
  unreconciled concept from tracker-announce errors.
- There is no "has tracker error" filter/view in the main Torrents list.
- The only way to find affected torrents is to click into each flagged
  tracker individually (11 of them) and cross-reference, which does not
  scale past a handful of trackers.

## Polish issues (low severity, quick fixes)

- **Category name encoding bug**: a category is stored/displayed as literal
  `linux%20iso` instead of `linux iso` — the `%20` was never decoded before
  being persisted as the category name. Visible in the sidebar, the
  Properties category dropdown, and Settings → Library → Categories.
- **Per-file progress shows 0% on a 100%-complete torrent**: Properties →
  Files tab shows a file's individual progress bar at 0% while the parent
  torrent shows 100.0% complete and fully seeding.
- **"Unsaved view" badge is always on**: it renders immediately after login
  with zero filters/search/sort changes applied — i.e., on the untouched
  default view — making the indicator meaningless as a "you have
  uncommitted changes" signal.
- **Double space in row title attribute**: row `title` attributes read
  `"Select  <name>"` (two spaces after "Select").
- **"Completed" timestamp semantics are ambiguous**: for a torrent added in
  2017, the detail panel's `COMPLETED` field shows a 2026 date — almost
  certainly the most recent recheck/verify time, not the original download
  completion time. Worth either renaming the label ("Last verified") or
  preserving the original completion date across rechecks.
- **No power-user keyboard shortcuts**: the in-app Help modal documents
  exactly two shortcuts (`A` = add torrent, `Esc` = close/clear). There's no
  select-all, delete, start/stop, or find-next binding — notable for a tool
  explicitly targeting 10k–100k-torrent, power-user-scale management.
- **Horizontal column overflow has no visible affordance**: at 1600px width
  with the default 17 visible columns, ~374px of columns (confirmed via
  `scrollWidth` vs `clientWidth`) sit past the right edge in a horizontally
  scrollable container with no visible scrollbar or "more columns" hint —
  discoverable only via the "Columns" button.

## Suggested priority order

1. P0-1 (mobile layout) and P0-3 (passkey masking) — both are "someone gets
   hurt" issues (unusable app / leaked credentials), not just polish.
2. P0-2 (WebSocket delta sync) — architectural debt that actively fights the
   project's own scale goals; also fixes the `/api/v1/events` 404 spam for
   free once the endpoint is either implemented or removed.
3. P0-4 (type inference) — either fix the heuristic or visually demote its
   confidence; it's currently actively misleading.
4. P1-2 (RPC trust flag) — likely a fast diagnosis given the Backend page
   already isolates it precisely.
5. P1-1 and P1-4 — worth a focused investigation pass each, since both were
   discovered from evidence (logs, ratio data) rather than direct root-cause
   access.
6. P1-3, P2-1..P2-4, and the polish list are all independently small,
   parallelizable fixes.

## Fix pass (2026-08-29)

Every finding above was fixed in source and verified either with a live
headless re-test against a local dev server proxied to the production
backend (frontend-only fixes — passkey masking, mobile layout, columns,
accessibility, the saved-view badge) or with `cargo test`/unit tests
(backend-only fixes — the SQL type-filter bug, category decoding, sync error
logging). **Nothing was deployed** — the sidecar binary and WebUI bundle on
`kspls0` still run the pre-fix build until it's rebuilt and redeployed.

| Finding | Status | Notes |
| --- | --- | --- |
| P0-1 Mobile layout | **Fixed** | Sidebar and status bar now collapse to a "Filters"/"More ▾" toggle on narrow viewports, defaulting closed — the table gets the vertical space by default instead of ~2 visible rows. Verified: 8 rows visible on load at 390×844 (was 0–2). |
| P0-2 No WebSocket | **Fixed** | The frontend unconditionally preferred a nonexistent SSE endpoint (`/api/v1/events`, 404) over the working `/ws` endpoint, because `EventSource` always exists in real browsers. Now always connects over `/ws`. Verified live against the *current deployed* backend — Conn/FW/DHT/PEX went from permanently "unknown"/"0" to real live values once the frontend fix loaded, since the backend's `/ws` support already existed and was simply unreachable. Full-refresh polling intervals reduced from 2s to 15–20s safety-net-only, now that push invalidation is the primary path. |
| P0-3 Passkey exposure | **Fixed** | New `TrackerUrl` component (`webui/src/lib/maskUrl.tsx`) masks credential query params and opaque path-segment passkeys by default, with a "Show"/"Copy" affordance, in the detail panel, Properties dialog, and Tracker Health panel. Verified live: `https://tracker.hdbits.org/announce.php?passkey=••••••••` etc. |
| P0-4 Type inference | **Fixed** | Root cause: the SQL `LIKE` pattern for "tv" included a literal `"s%e%"` glob meant to approximate SxxExx markers — in SQL `LIKE`, that matches any string containing an 's' anywhere before an 'e' anywhere later, i.e. most English text. Replaced the whole `LIKE`-glob classifier with a word-boundary-aware Rust classifier (`sidecar/src/media_type.rs`) registered as a SQLite scalar function, with a real SxxExx digit-run detector. Unit-tested against the exact false positives found live (the Grisham ebook, the FLAC album, and the `Empire Games` ebook game-title collision). Requires a sidecar rebuild to take effect. |
| P1-1 Sync reliability | **Improved** | Two changes: (1) error logging previously called `.to_string()`/`%e` on the anyhow chain, which only prints the outermost `.context()` layer ("d.multicall.range offset=X limit=100") and silently drops the actual XMLRPC fault underneath — the one detail needed to diagnose *why*. Now logs the full chain. (2) `tick_bounded` now retries a failed 100-torrent range by bisecting it instead of losing the whole page for that cycle, isolating the fault to the specific torrent(s) involved. Root cause of the underlying XMLRPC faults is still unconfirmed — these changes make it observable and non-disruptive rather than fixing an unknown upstream cause. |
| P1-2 RPC trust flag | **Fixed** | The backend now cross-checks `load.start`/`load.raw_start` availability when explaining why `rpc.trusted_connection_accept_all.set` is unavailable, since the sidecar's trusted local-SCGI-socket connection makes that broader toggle unnecessary when those two calls work. Separately: the frontend computed this `detail` text but never rendered it anywhere — now it does, for all capabilities, not just this one. |
| P1-3 Pagination | **Already fixed in `main`** | Infinite scroll (500px-from-bottom trigger) was already implemented in source; the deployed build the original audit ran against just predated it. No change needed. |
| P1-4 Ratio 0.00 | **Reassessed, not a code bug** | The original hypothesis (migration data-loss) assumed the wrong runtime track — this deployment is the rTorrent-backed sidecar, which never goes through `rt-migrate` at all; ratio comes straight from rTorrent's own resume state. The more likely explanation is the "Conn 0 / FW unknown / DHT unknown / PEX unknown" seen throughout the audit — which turned out to be a **direct symptom of P0-2** (those fields are WS-only and were simply never arriving). With P0-2 fixed, they'll show real values, which is the right next step for diagnosing whether it's a genuine connectivity issue (port forwarding, firewall) rather than a TorrentNG bug. |
| P2-1 Accessibility | **Fixed** | `color-contrast`: the default theme's toolbar/detail-panel action buttons blended text color toward `--text` instead of using the raw (4.39:1) accent tint. Also fixed an unrelated bug hit while in this code: `` `1px solid ${color}55` `` where `color` is a `var(--token)` reference, not a hex string — `"var(--accent)55"` is not a valid CSS color and the whole border declaration was silently dropped by the browser. `landmark-unique`/`region`/`page-has-heading-one`: added `aria-label`s to the two unlabeled `<aside>`s, wrapped the filter/toolbar controls in a `<nav>`, added an `<h1>`. Verified: 0 axe-core violations (was 1 serious + 3 moderate). |
| P2-2 Facets vs. search | **Fixed** | `sidebar_facets` took no query params at all — it was always an unfiltered, whole-library aggregate. It now accepts the same search/category/tag/tracker filters as the main list query and applies them to both the STATE and TYPE buckets (excluding each bucket's own dimension, so filtering by a status doesn't collapse its own facet to itself). |
| P2-3 Settings IA | **Fixed** | Moved the raw Operator Logs panel from "Support" (mixed with theme picker and Discord/GitHub links) to "Backend" (next to the sync-error diagnostics it explains). |
| P2-4 Tracker-error findability | **Fixed** | Tracker Health rows are now clickable ("View torrents →"), jumping to the torrent list pre-filtered to that tracker. Doesn't isolate the specific erroring torrents within a tracker (would need a schema change to track per-torrent tracker error state) — noted as a follow-up, not implemented here. |
| Polish: `linux%20iso` category | **Fixed** | Root cause: classic ruTorrent stores label/category values in `d.custom1` using PHP `rawurlencode()`; TorrentNG read it raw. Now decodes defensively (only when the value contains a `%XX` escape and round-trips to valid UTF-8, so a category with a literal `%` isn't mangled). Requires a sidecar rebuild to take effect on already-migrated data (existing rows aren't retroactively renamed — only what's read from rTorrent going forward). |
| Polish: file shows 0% | **Fixed** | Defensive fix: a wanted file (priority ≠ 0) in a torrent the backend already reports complete can't itself be incomplete — trust `torrent.complete` over a stale/zero per-file chunk count. Root cause in the rTorrent XMLRPC layer not fully isolated (the query code looked structurally correct). |
| Polish: "Unsaved view" always on | **Fixed** | Three compounding bugs, found by instrumenting the actual runtime values: (1) key-order-sensitive `JSON.stringify` comparison; (2) the debounced search effect in `FilterBar` fires once on mount even with empty input, adding a real `offset: 0` key to `params` that `cleanParams` wasn't stripping (`offset`/`limit` are pagination position, not filter identity); (3) the server round-trips a saved view's unset fields as JSON `null`, which `cleanParams`'s `undefined`/`''` check didn't catch. Verified: badge is off on a pristine load and on with a real filter applied. |
| Polish: login form attrs | **Fixed** | Added `id`/`name` to both inputs (the `autocomplete` attributes were already present, contrary to the original finding — only `id`/`name` were actually missing). |
| Not yet in the audit doc: keyboard shortcuts | **Fixed** | Added `Delete` (delete the single selected torrent, with confirmation) and `Ctrl/⌘+A` (select all loaded torrents); documented in the Help dialog along with the `?` shortcut itself, which was usable but undocumented. |

### New: column resize, reorder, and pin (user-reported, not in the original audit)

The user reported columns "don't resize, re-order, stick, or pull" on the
live site. Investigation found the table's columns were fixed-width,
fixed-order, with no pinning at all — `TorrentTable.tsx` rewritten to add:

- **Resize**: a drag handle on each column's right edge, min 44px, persisted
  to `localStorage` (`tng.columnWidths.v1`).
- **Reorder**: drag-and-drop column headers (native HTML5 DnD, no library),
  persisted (`tng.columnOrder.v1`). The checkbox and the trailing
  Columns-menu spacer column stay fixed at the ends; everything else is
  reorderable.
- **Pin ("stick")**: Type, Name, and the selection checkbox stay pinned to
  the left edge while scrolling horizontally through the rest, with a
  divider shadow. "Sticky header while scrolling vertically" turned out to
  already work via the existing layout (flex column with the header outside
  the scrolling body) — this and the new left-pinning are both needed for
  "pull" (scrolling the table sideways with key columns staying put).

Getting sticky-left working exposed a real, somewhat subtle CSS bug worth
recording: the table split horizontal and vertical scrolling across two
nested elements (`overflow-x: auto` outer, `overflow-y: auto` inner). Per the
CSS Overflow spec, setting `overflow-y` to anything but `visible` forces the
paired `overflow-x: visible` to compute to `auto` — so the *inner* (vertical
only) element silently became the sticky containing block instead of the
*outer* (horizontal) one, and every sticky-left offset was computed against
an axis that never actually scrolled. Fixed by merging both axes onto one
element (`overflow: auto` on the single scroll container, header inside it
as `position: sticky; top: 0`, matching a standard virtualized+sticky-header
pattern) rather than trying to keep them on two elements. Verified via
computed-style inspection (`getComputedStyle(...).left` went from consistent
negative/off-screen values to correct positive on-screen ones) and a live
drag-resize/drag-reorder/horizontal-scroll test.

### Independent release review (2026-08-31)

The pre-release review found and corrected several gaps in the first fix pass:

- A failed page earlier in a bounded-sync cycle could still be followed by a
  clean short page and cause the cache to delete the omitted torrent. Cleanup
  now runs only after a completely clean cycle, then resets the cycle state.
- The documented Ctrl/⌘+A shortcut was unreachable because the plain `A`
  handler ran first. Modifier-aware dispatch and a regression test now keep
  select-all separate from Add.
- Passive tracker displays now mask credentials in table/health hover titles,
  tracker filter inputs/chips, and saved-view metadata, in addition to the
  visible URL components. Explicit Show/Copy/Edit controls remain deliberate
  reveal paths.
- SQL media filtering now includes tags, matching the WebUI's Full inference
  mode, and season/episode detection requires boundaries around the complete
  marker rather than accepting a prefix of a longer token.
- Sticky-column offsets now include the table's horizontal padding and grid
  gaps, and Type/Name are structurally kept at the left identity edge even
  after other columns are reordered.

### Verification method

Frontend fixes were checked against real production data via a local Vite
dev server (`webui/`) with `/api` and `/ws` proxied to
`https://rutorrent-next.home` over HTTPS — read-only the whole time; no
mutating requests beyond the login itself were made from the test harness.
Backend fixes were checked with `cargo check`, the existing 51-test suite
(all passing) plus classifier/category/tag regression coverage, run under an
explicit toolchain (`rustup run 1.97.0 cargo test`) since the default `stable`
toolchain on this machine was missing its `rustc`/`cargo` components. The
sidecar run executed 127 tests; the WebUI certification run passed its desktop
and mobile functional, accessibility, and scale checks, with workspace visual
baselines regenerated for the intentional layout changes.
