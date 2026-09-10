# webui/

React + TypeScript + Vite frontend. It is backend-agnostic by design: the
same UI talks to either engine track over the same API shapes — native
`torrentngd` REST/SSE, or the rTorrent-backed sidecar's REST/WS — so switching
which backend is running underneath does not require a different frontend
build.

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
| `src/api/` | Typed API client for native and compat endpoints (`client.ts`) |
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
`http://localhost:8080` — start whichever backend you want to develop against
on that port first (see the root [README](../README.md#quick-start) for
native, or [rTorrent Mode](../README.md#rtorrent-mode) for the sidecar).

`npm run build` outputs to `../sidecar/static` by default (what the Track 1
Docker image ships). Native builds override this with
`TNG_WEBUI_OUT_DIR=dist npm run build` — see `deploy/native/Dockerfile`.

## Related

- [docs/API.md](../docs/API.md) — native and compatibility API surfaces this
  UI consumes
- [docs/WEBUI_AUDIT.md](../docs/WEBUI_AUDIT.md) — table alignment, status
  semantics, and known cross-client projection gaps
