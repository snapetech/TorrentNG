import { useQuery, useInfiniteQuery } from '@tanstack/react-query'
import { api, type ListParams, type TorrentSummary } from '../api/client'

const PAGE_SIZE = 200

export function useTorrentsInfinite(params: Omit<ListParams, 'limit' | 'offset'>, enabled = true) {
  return useInfiniteQuery({
    queryKey: ['torrents', params],
    queryFn: ({ pageParam }) =>
      api.torrents.list({
        ...params,
        limit: PAGE_SIZE,
        offset: pageParam.offset,
        snapshot: pageParam.snapshot,
      }),
    enabled,
    initialPageParam: { offset: 0 } as { offset: number; snapshot?: number },
    getNextPageParam: (lastPage, allPages) => {
      const loaded = allPages.reduce((n, p) => n + p.torrents.length, 0)
      return loaded < lastPage.total
        ? { offset: loaded, snapshot: lastPage.snapshot }
        : undefined
    },
    placeholderData: (prev) => prev,
    staleTime: 1000,
    // Real-time updates are pushed over the WebSocket (see useWebSocket),
    // which invalidates this query key on torrent add/remove/update. This
    // interval is only a safety net for when the socket is down (e.g. a
    // proxy that blocks WS upgrades), so it can be slow.
    refetchInterval: enabled ? 20_000 : false,
  })
}

/** Flatten infinite query pages into a single torrent array + total count. */
export function flattenPages(data: ReturnType<typeof useTorrentsInfinite>['data']) {
  if (!data) return { torrents: [] as TorrentSummary[], total: 0 }
  const torrents = data.pages.flatMap(p => p.torrents)
  const total = data.pages[0]?.total ?? 0
  return { torrents, total }
}

export function useHealth(enabled = true) {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    enabled,
    refetchInterval: 10_000,
  })
}
