import { useEffect, useRef } from 'react'
import { useQueryClient } from '@tanstack/react-query'

interface WsEvent {
  type: 'torrent_added' | 'torrent_removed' | 'torrent_updated' | 'stats'
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
          // Any torrent change — invalidate the list. Batched by browser animation frame.
          qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
          if (msg.hash) {
            qc.invalidateQueries({ queryKey: ['trackers', msg.hash] })
            qc.invalidateQueries({ queryKey: ['files', msg.hash] })
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
