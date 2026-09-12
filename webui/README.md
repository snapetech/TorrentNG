# webui/

React + TypeScript + Vite frontend. It is backend-agnostic by design: the
same UI talks to the TorrentNG client (`torrentngd`) directly or to the
TorrentNG WebUI/API service connected to a compatible client such as rTorrent,
qBittorrent, Transmission, or Deluge. Switching the transfer client does not
require a different frontend build.

TorrentNG is the user-facing WebUI and automation surface. A compatible client
continues to own transfer and session state, while `torrentngd` is TorrentNG's
next-generation first-party client with its own storage, persistence, job, and
protocol implementation.

## Constraints that shape this codebase

- **Must handle 100k rows.** The torrent table is virtualized
  (`@tanstack/react-virtual`) — never render or hold a non-virtualized list of
  all torrents.
- **Server-side sort/filter/paginate.** Filtering and sorting happen on
  whichever backend cache is running; the browser never loads the full
  torrent set to filter client-side.
- **Delta sync, not polling.** Live updates come over WebSocket/SSE deltas,
  not full-refresh polling loops.
- **No right-click dependency.** Every action needs a non-context-menu path
  for mobile/touch support.

## Layout

| Path | Purpose |
|---|---|
| `src/api/` | Typed API client for TorrentNG and compatible-client endpoints (`client.ts`) |
| `src/components/` | Reusable UI components (table, panels, forms) |
| `src/hooks/` | TanStack Query hooks and other stateful logic |
| `src/views/` | Top-level routed views |
| `src/lib/` | Shared utilities |

Server state lives in TanStack Query — don't reach for a separate global
store for data an API already owns.

## Develop

```sh
cd webui
npm install
npm run dev       # Vite dev server
npm run build     # tsc + production build
npm run lint      # eslint, zero warnings allowed
npm run test:e2e  # Playwright browser tests
```

The dev server (`vite.config.ts`) proxies `/api` and `/ws` to
`http://localhost:8080` — start whichever TorrentNG service you want to
develop against on that port first (see the root README's
[TorrentNG client quick start](../README.md#quick-start-torrentng-client), or
[compatible-client integration](../README.md#compatible-client-integration)).

`npm run build` outputs to `../sidecar/static` by default (the
compatible-client Docker image ships these assets). TorrentNG-client builds
override this with
`TNG_WEBUI_OUT_DIR=dist npm run build` — see `deploy/native/Dockerfile`.

## Related

- [docs/API.md](../docs/API.md) — TorrentNG and compatibility API surfaces this
  UI consumes
- [docs/WEBUI_AUDIT.md](../docs/WEBUI_AUDIT.md) — table alignment, status
  semantics, and known cross-client projection gaps
