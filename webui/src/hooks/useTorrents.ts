import { useQuery, useInfiniteQuery } from '@tanstack/react-query'
import { useEffect, useMemo, useRef, useState } from 'react'
import { api, type ListParams, type LiveTorrentStatsResponse, type TorrentSummary } from '../api/client'

const PAGE_SIZE = 200
const LIVE_STATS_INTERVAL_MS = 2500
const MAX_VISIBLE_LIVE_TORRENTS = 128

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

/**
 * Refresh live rates only for the torrent hashes supplied by the virtualized
 * table. The query pauses while the document is hidden and is capped so a
 * pathological viewport cannot create an unbounded request.
 */
export function useLiveTorrentStats(hashes: string[], enabled = true) {
  const normalizedHashes = useMemo(
    () => [...new Set(hashes)].sort().slice(0, MAX_VISIBLE_LIVE_TORRENTS),
    [hashes],
  )
  const [tabVisible, setTabVisible] = useState(
    () => typeof document === 'undefined' || document.visibilityState === 'visible',
  )

  useEffect(() => {
    if (typeof document === 'undefined') return
    const onVisibilityChange = () => setTabVisible(document.visibilityState === 'visible')
    document.addEventListener('visibilitychange', onVisibilityChange)
    return () => document.removeEventListener('visibilitychange', onVisibilityChange)
  }, [])

  return useQuery({
    queryKey: ['torrent-live-stats', normalizedHashes],
    queryFn: () => api.torrents.liveStats(normalizedHashes),
    enabled: enabled && tabVisible && normalizedHashes.length > 0,
    staleTime: 1500,
    refetchInterval: enabled && tabVisible ? LIVE_STATS_INTERVAL_MS : false,
    refetchIntervalInBackground: false,
    refetchOnWindowFocus: true,
    placeholderData: (previous) => previous,
  })
}

export interface SmoothedLiveRate {
  amountLeft: number
  downloadRate: number
  uploadRate: number
  fresh: boolean
}

interface RateSample {
  amountLeft: number
  downloadRate: number
  uploadRate: number
  sampledAt: number
}

/** Keep a short client-side mean so ETA does not jump on every backend tick. */
export function useSmoothedLiveRates(data: LiveTorrentStatsResponse | undefined) {
  const historyRef = useRef(new Map<string, RateSample[]>());
  const [revision, setRevision] = useState(0)
  const [clock, setClock] = useState(0)
  const hasSamples = Boolean(data && data.torrents.length > 0)

  useEffect(() => {
    if (!hasSamples) return
    const updateClock = () => setClock(Date.now())
    updateClock()
    const timer = window.setInterval(updateClock, LIVE_STATS_INTERVAL_MS)
    return () => window.clearInterval(timer)
  }, [hasSamples])

  useEffect(() => {
    if (!data || data.torrents.length === 0) return
    const receivedAt = Date.now()
    for (const stat of data.torrents) {
      const samples = historyRef.current.get(stat.hash) ?? []
      const sampledAt = Number.isFinite(stat.sampled_at) && stat.sampled_at > 0
        ? stat.sampled_at
        : receivedAt
      const previous = samples[samples.length - 1]
      if (previous && previous.sampledAt === sampledAt && previous.amountLeft === stat.amount_left
        && previous.downloadRate === stat.download_rate && previous.uploadRate === stat.upload_rate) {
        continue
      }
      samples.push({
        amountLeft: Math.max(0, stat.amount_left),
        downloadRate: Math.max(0, stat.download_rate),
        uploadRate: Math.max(0, stat.upload_rate),
        sampledAt,
      })
      historyRef.current.set(stat.hash, samples.slice(-4))
    }

    const cutoff = receivedAt - 30_000
    for (const [hash, samples] of historyRef.current) {
      const recent = samples.filter(sample => sample.sampledAt >= cutoff)
      if (recent.length === 0) historyRef.current.delete(hash)
      else historyRef.current.set(hash, recent)
    }
    while (historyRef.current.size > 512) {
      const oldest = historyRef.current.keys().next().value
      if (!oldest) break
      historyRef.current.delete(oldest)
    }
    setRevision(value => value + 1)
  }, [data])

  return useMemo(() => {
    const rates = new Map<string, SmoothedLiveRate>()
    for (const [hash, samples] of historyRef.current) {
      if (samples.length === 0) continue
      const latest = samples[samples.length - 1]
      const downloadRate = samples.reduce((sum, sample) => sum + sample.downloadRate, 0) / samples.length
      const uploadRate = samples.reduce((sum, sample) => sum + sample.uploadRate, 0) / samples.length
      rates.set(hash, {
        amountLeft: latest.amountLeft,
        downloadRate,
        uploadRate,
        fresh: clock === 0 || clock - latest.sampledAt <= LIVE_STATS_INTERVAL_MS * 3,
      })
    }
    return rates
  }, [clock, data, revision])
}

export function useHealth(enabled = true) {
  return useQuery({
    queryKey: ['health'],
    queryFn: api.health,
    enabled,
    refetchInterval: 10_000,
  })
}
