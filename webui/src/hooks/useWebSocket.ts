import { useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'

interface WsEvent {
  type:
    | 'torrent_added'
    | 'torrent_removed'
    | 'torrent_updated'
    | 'categories_updated'
    | 'tags_updated'
    | 'tracker_health_updated'
    | 'storage_updated'
    | 'ratio_groups_updated'
    | 'workflows_updated'
    | 'workflow_runs_updated'
    | 'rss_rules_updated'
    | 'saved_views_updated'
    | 'stats'
  hash?: string
  upload_speed?: number
  download_speed?: number
}

export function useWebSocket(onStats?: (up: number, dn: number) => void, enabled = true) {
  const qc = useQueryClient()
  const ws = useRef<WebSocket | null>(null)
  const statsRef = useRef(onStats)
  const torrentsInvalidationTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const detailInvalidationTimer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const detailHashes = useRef<Set<string>>(new Set())
  statsRef.current = onStats

  useEffect(() => {
    if (!enabled) return

    let reconnectTimer: ReturnType<typeof setTimeout>
    let closed = false
    let reconnectDelayMs = 3000

    function scheduleTorrentInvalidation(hash?: string) {
      if (hash) detailHashes.current.add(hash)
      if (!torrentsInvalidationTimer.current) {
        torrentsInvalidationTimer.current = setTimeout(() => {
          torrentsInvalidationTimer.current = null
          qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
        }, 1000)
      }
      if (hash && !detailInvalidationTimer.current) {
        detailInvalidationTimer.current = setTimeout(() => {
          const hashes = [...detailHashes.current]
          detailHashes.current.clear()
          detailInvalidationTimer.current = null
          for (const h of hashes) {
            qc.invalidateQueries({ queryKey: ['trackers', h] })
            qc.invalidateQueries({ queryKey: ['files', h] })
          }
        }, 1500)
      }
    }

    function connect() {
      const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`
      const socket = new WebSocket(url)
      ws.current = socket
      socket.onopen = () => {
        reconnectDelayMs = 3000
      }

      socket.onmessage = (e) => {
        try {
          const msg: WsEvent = JSON.parse(e.data)
          if (msg.type === 'stats') {
            statsRef.current?.(msg.upload_speed ?? 0, msg.download_speed ?? 0)
            return
          }
          switch (msg.type) {
            case 'torrent_added':
            case 'torrent_removed':
            case 'torrent_updated':
              scheduleTorrentInvalidation(msg.hash)
              break
            case 'categories_updated':
              qc.invalidateQueries({ queryKey: ['categories'] })
              scheduleTorrentInvalidation()
              break
            case 'tags_updated':
              qc.invalidateQueries({ queryKey: ['tags'] })
              scheduleTorrentInvalidation()
              break
            case 'tracker_health_updated':
              qc.invalidateQueries({ queryKey: ['tracker-health'] })
              break
            case 'storage_updated':
              qc.invalidateQueries({ queryKey: ['storage'] })
              break
            case 'ratio_groups_updated':
              qc.invalidateQueries({ queryKey: ['ratio-groups'] })
              break
            case 'workflows_updated':
              qc.invalidateQueries({ queryKey: ['workflows'] })
              break
            case 'workflow_runs_updated':
              qc.invalidateQueries({ queryKey: ['workflow-runs'] })
              break
            case 'rss_rules_updated':
              qc.invalidateQueries({ queryKey: ['rss-rules'] })
              break
            case 'saved_views_updated':
              qc.invalidateQueries({ queryKey: ['saved-views'] })
              break
          }
        } catch {
          // malformed event — ignore
        }
      }

      socket.onclose = () => {
        if (!closed) {
          reconnectTimer = setTimeout(connect, reconnectDelayMs)
          reconnectDelayMs = Math.min(reconnectDelayMs * 2, 30_000)
        }
      }
    }

    connect()
    return () => {
      closed = true
      clearTimeout(reconnectTimer)
      if (torrentsInvalidationTimer.current) clearTimeout(torrentsInvalidationTimer.current)
      if (detailInvalidationTimer.current) clearTimeout(detailInvalidationTimer.current)
      torrentsInvalidationTimer.current = null
      detailInvalidationTimer.current = null
      detailHashes.current.clear()
      ws.current?.close()
    }
  }, [qc, enabled])
}
