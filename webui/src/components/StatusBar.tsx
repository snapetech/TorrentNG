import type { LiveStats, StorageRoot } from '../api/client'

interface Props {
  loaded: number
  total: number
  selected: number
  stats: LiveStats
  rtorrent: string
  cached?: number
  storage?: StorageRoot
}

function fmtSpeed(bps: number): string {
  if (!bps) return '0 B/s'
  if (bps >= 1e9) return (bps / 1e9).toFixed(1) + ' GB/s'
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' MB/s'
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' KB/s'
  return bps + ' B/s'
}

function fmtBytes(bytes?: number): string {
  if (bytes === undefined || !Number.isFinite(bytes)) return 'unknown'
  if (bytes >= 1e12) return (bytes / 1e12).toFixed(1) + ' TB'
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(1) + ' GB'
  if (bytes >= 1e6) return (bytes / 1e6).toFixed(1) + ' MB'
  return Math.max(0, bytes).toLocaleString() + ' B'
}

function tone(value?: string): { color: string; bg: string; border: string } {
  if (value === 'open' || value === 'on') return { color: '#86efac', bg: 'rgba(34,197,94,.12)', border: 'rgba(34,197,94,.35)' }
  if (value === 'listening' || value === 'unknown') return { color: '#fde68a', bg: 'rgba(245,158,11,.12)', border: 'rgba(245,158,11,.35)' }
  if (value === 'closed') return { color: '#fca5a5', bg: 'rgba(239,68,68,.12)', border: 'rgba(239,68,68,.35)' }
  return { color: '#94a3b8', bg: 'rgba(148,163,184,.08)', border: 'rgba(148,163,184,.22)' }
}

function Badge({ label, value, title, state }: { label: string; value: string; title?: string; state?: string }) {
  const t = tone(state)
  return (
    <span title={title} style={{
      display: 'inline-flex',
      alignItems: 'center',
      gap: 5,
      minHeight: 22,
      padding: '0 7px',
      border: `1px solid ${t.border}`,
      borderRadius: 6,
      background: t.bg,
      color: t.color,
      whiteSpace: 'nowrap',
      fontVariantNumeric: 'tabular-nums',
    }}>
      <span style={{ color: '#64748b' }}>{label}</span>
      <span>{value}</span>
    </span>
  )
}

export function StatusBar({ loaded, total, selected, stats, rtorrent, cached, storage }: Props) {
  const connected = rtorrent === 'connected'
  const rendered = Math.min(loaded, total)
  const storageLabel = storage?.ok
    ? `${fmtBytes(storage.used_bytes)} / ${fmtBytes(storage.total_bytes)}`
    : 'unknown'
  const storageTitle = storage?.ok
    ? `${fmtBytes(storage.available_bytes)} free on ${storage.path}`
    : storage?.error ?? 'Storage status unavailable'

  return (
    <footer style={{
      minHeight: 36, flexShrink: 0, display: 'flex', alignItems: 'center', gap: 12,
      padding: '0 12px', background: '#0d1117', borderTop: '1px solid #273244',
      color: '#64748b', fontSize: 11, overflowX: 'auto',
    }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 'max-content' }}>
        <Badge label="Core" value={connected ? 'connected' : 'disconnected'} state={connected ? 'on' : 'closed'} />
        <span>{total.toLocaleString()} torrents</span>
        {rendered !== total && <span>{rendered.toLocaleString()} rendered</span>}
        {cached !== undefined && <span>{cached.toLocaleString()} cached</span>}
        {selected > 0 && <span style={{ color: '#93c5fd' }}>{selected.toLocaleString()} selected</span>}
      </div>
      <span style={{ flex: 1 }} />
      <div style={{ display: 'flex', alignItems: 'center', gap: 7, minWidth: 'max-content' }}>
        <Badge label="DL" value={fmtSpeed(stats.download_speed)} state={(stats.download_speed ?? 0) > 0 ? 'on' : 'unknown'} />
        <Badge label="UL" value={fmtSpeed(stats.upload_speed)} state={(stats.upload_speed ?? 0) > 0 ? 'on' : 'unknown'} />
        <Badge label="Disk" value={storageLabel} title={storageTitle} state={storage?.ok ? 'on' : 'unknown'} />
        <Badge
          label="Conn"
          value={`${(stats.connections ?? 0).toLocaleString()}${stats.pending_connections ? ` +${stats.pending_connections}` : ''}`}
          title="Established and pending peer sockets on the incoming port"
          state={(stats.connections ?? 0) > 0 ? 'on' : 'unknown'}
        />
        <Badge
          label="FW"
          value={`${stats.firewall ?? 'unknown'}${stats.listen_port ? ` :${stats.listen_port}` : ''}`}
          title="Incoming port listener state inferred from live TCP sockets"
          state={stats.firewall}
        />
        <Badge label="DHT" value={stats.dht ?? 'unknown'} title="Runtime rTorrent DHT config" state={stats.dht} />
        <Badge label="PEX" value={stats.pex ?? 'unknown'} title="Runtime rTorrent peer exchange config" state={stats.pex} />
      </div>
    </footer>
  )
}
