interface Props {
  loaded: number
  total: number
  selected: number
  up: number
  down: number
  rtorrent: string
  cached?: number
}

function fmtSpeed(bps: number): string {
  if (!bps) return '0 B/s'
  if (bps >= 1e9) return (bps / 1e9).toFixed(1) + ' GB/s'
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' MB/s'
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' KB/s'
  return bps + ' B/s'
}

export function StatusBar({ loaded, total, selected, up, down, rtorrent, cached }: Props) {
  const connected = rtorrent === 'connected'
  return (
    <footer style={{
      height: 28, flexShrink: 0, display: 'flex', alignItems: 'center', gap: 14,
      padding: '0 12px', background: '#0d1117', borderTop: '1px solid #1e2433',
      color: '#64748b', fontSize: 11,
    }}>
      <span style={{ color: connected ? '#22c55e' : '#ef4444' }}>
        {connected ? 'Connected' : 'Disconnected'}
      </span>
      <span>{loaded.toLocaleString()} loaded / {total.toLocaleString()} shown</span>
      {cached !== undefined && <span>{cached.toLocaleString()} cached</span>}
      {selected > 0 && <span style={{ color: '#93c5fd' }}>{selected.toLocaleString()} selected</span>}
      <span style={{ marginLeft: 'auto', color: '#3b82f6' }}>Down {fmtSpeed(down)}</span>
      <span style={{ color: '#22c55e' }}>Up {fmtSpeed(up)}</span>
    </footer>
  )
}
