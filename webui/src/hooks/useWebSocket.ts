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

export function useWebSocket(onStats?: (up: number, dn: number) => void) {
  const qc = useQueryClient()
  const ws = useRef<WebSocket | null>(null)
  const statsRef = useRef(onStats)
  statsRef.current = onStats

  useEffect(() => {
    let reconnectTimer: ReturnType<typeof setTimeout>
    let closed = false

    function connect() {
      const url = `${location.protocol === 'https:' ? 'wss' : 'ws'}://${location.host}/ws`
      const socket = new WebSocket(url)
      ws.current = socket

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
              qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
              if (msg.hash) {
                qc.invalidateQueries({ queryKey: ['trackers', msg.hash] })
                qc.invalidateQueries({ queryKey: ['files', msg.hash] })
              }
              break
            case 'categories_updated':
              qc.invalidateQueries({ queryKey: ['categories'] })
              qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
              break
            case 'tags_updated':
              qc.invalidateQueries({ queryKey: ['tags'] })
              qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
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
        if (!closed) reconnectTimer = setTimeout(connect, 3000)
      }
    }

    connect()
    return () => {
      closed = true
      clearTimeout(reconnectTimer)
      ws.current?.close()
    }
  }, [qc])
}
