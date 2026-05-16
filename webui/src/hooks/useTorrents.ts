import { useQuery, useInfiniteQuery } from '@tanstack/react-query'
import { api, type ListParams, type TorrentSummary } from '../api/client'

const PAGE_SIZE = 200

export function useTorrentsInfinite(params: Omit<ListParams, 'limit' | 'offset'>) {
  return useInfiniteQuery({
    queryKey: ['torrents', params],
    queryFn: ({ pageParam = 0 }) =>
      api.torrents.list({ ...params, limit: PAGE_SIZE, offset: pageParam }),
    initialPageParam: 0,
    getNextPageParam: (lastPage, allPages) => {
      const loaded = allPages.reduce((n, p) => n + p.torrents.length, 0)
      return loaded < lastPage.total ? loaded : undefined
    },
    placeholderData: (prev) => prev,
    staleTime: 3000,
  })
}

/** Flatten infinite query pages into a single torrent array + total count. */
export function flattenPages(data: ReturnType<typeof useTorrentsInfinite>['data']) {
  if (!data) return { torrents: [] as TorrentSummary[], total: 0 }
  const torrents = data.pages.flatMap(p => p.torrents)
  const total = data.pages[0]?.total ?? 0
  return { torrents, total }
}

export function useHealth() {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    refetchInterval: 10_000,
  })
}
