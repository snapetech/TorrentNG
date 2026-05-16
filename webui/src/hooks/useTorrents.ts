import { useQuery, useInfiniteQuery } from '@tanstack/react-query'
import { api, type ListParams, type TorrentSummary } from '../api/client'

const PAGE_SIZE = 200

export function useTorrentsInfinite(params: Omit<ListParams, 'limit' | 'offset'>, enabled = true) {
  return useInfiniteQuery({
    queryKey: ['torrents', params],
    queryFn: ({ pageParam = 0 }) =>
      api.torrents.list({ ...params, limit: PAGE_SIZE, offset: pageParam }),
    enabled,
    initialPageParam: 0,
    getNextPageParam: (lastPage, allPages) => {
      const loaded = allPages.reduce((n, p) => n + p.torrents.length, 0)
      return loaded < lastPage.total ? loaded : undefined
    },
    placeholderData: (prev) => prev,
    staleTime: 1000,
    refetchInterval: enabled ? 2_000 : false,
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
