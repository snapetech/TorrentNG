import type { LiveStats, StorageRoot } from '../api/client'

interface Props {
  loaded: number
  total: number
  selected: number
  stats: LiveStats
  rtorrent: string
  cached?: number
  storage?: StorageRoot
  togglingFeature?: 'dht' | 'pex' | null
  featureError?: string | null
  actionMessage?: string | null
  actionTone?: 'ok' | 'error'
  onToggleDht?: () => void
  onTogglePex?: () => void
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
  if (value === 'listening' || value === 'unknown') return { color: '#fbbf24', bg: 'rgba(245,158,11,.14)', border: 'rgba(245,158,11,.5)' }
  if (value === 'closed' || value === 'off') return { color: '#fca5a5', bg: 'rgba(239,68,68,.14)', border: 'rgba(239,68,68,.5)' }
  return { color: '#94a3b8', bg: 'rgba(148,163,184,.08)', border: 'rgba(148,163,184,.22)' }
}

function Badge({
  label, value, title, state, onClick, disabled,
}: { label: string; value: string; title?: string; state?: string; onClick?: () => void; disabled?: boolean }) {
  const t = tone(state)
  const Element = onClick && !disabled ? 'button' : 'span'
  const handleClick = disabled ? undefined : onClick
  return (
    <Element title={title} onClick={handleClick} style={{
      display: 'inline-flex',
      alignItems: 'center',
      gap: 5,
      minHeight: 22,
      padding: '0 8px',
      border: `1px solid ${t.border}`,
      borderRadius: 6,
      background: t.bg,
      color: t.color,
      whiteSpace: 'nowrap',
      fontVariantNumeric: 'tabular-nums',
      cursor: onClick && !disabled ? 'pointer' : 'default',
      font: 'inherit',
      opacity: disabled ? 0.7 : 1,
    }}>
      <span style={{ color: 'var(--faint)' }}>{label}</span>
      <span>{value}</span>
    </Element>
  )
}

export function StatusBar({
  loaded, total, selected, stats, rtorrent, cached, storage, togglingFeature, onToggleDht, onTogglePex,
  featureError, actionMessage, actionTone = 'ok',
}: Props) {
  const connected = rtorrent === 'connected'
  const rendered = Math.min(loaded, total)
  const storageLabel = storage?.ok
    ? `${fmtBytes(storage.used_bytes)} / ${fmtBytes(storage.total_bytes)}`
    : 'unknown'
  const storageTitle = storage?.ok
    ? `${fmtBytes(storage.available_bytes)} free on ${storage.path}`
    : storage?.error ?? 'Storage status unavailable'

  return (
    <footer className="rtng-statusbar" style={{
      minHeight: 38, flexShrink: 0, display: 'flex', alignItems: 'center', gap: 12,
      padding: '0 12px', background: 'var(--bg)', borderTop: '1px solid var(--border-strong)',
      color: 'var(--faint)', fontSize: 11, overflowX: 'auto',
    }}>
      <div className="rtng-statusbar-summary" style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 'max-content' }}>
        <Badge label="Core" value={connected ? 'connected' : 'disconnected'} state={connected ? 'on' : 'closed'} />
      </div>
      <div className="rtng-statusbar-counts" style={{ display: 'flex', alignItems: 'center', gap: 8, minWidth: 'max-content' }}>
        <span>{total.toLocaleString()} torrents</span>
        {rendered !== total && <span>{rendered.toLocaleString()} rendered</span>}
        {cached !== undefined && <span>{cached.toLocaleString()} cached</span>}
        {selected > 0 && <span style={{ color: 'var(--accent)' }}>{selected.toLocaleString()} selected</span>}
        {featureError && <span style={{ color: 'var(--danger)' }}>{featureError}</span>}
        {actionMessage && (
          <span style={{ color: actionTone === 'error' ? 'var(--danger)' : 'var(--success)' }}>
            {actionMessage}
          </span>
        )}
      </div>
      <span className="rtng-statusbar-spacer" style={{ flex: 1 }} />
      <div className="rtng-statusbar-metrics" style={{ display: 'flex', alignItems: 'center', gap: 7, minWidth: 'max-content' }}>
        <Badge label="DL" value={fmtSpeed(stats.download_speed)} state={(stats.download_speed ?? 0) > 0 ? 'on' : 'unknown'} />
        <Badge label="UL" value={fmtSpeed(stats.upload_speed)} state={(stats.upload_speed ?? 0) > 0 ? 'on' : 'unknown'} />
        <Badge label="DL total" value={fmtBytes(stats.download_total)} title="Downloaded during this daemon session" state="on" />
        <Badge label="UL total" value={fmtBytes(stats.upload_total)} title="Uploaded during this daemon session" state="on" />
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
        <Badge
          label="DHT"
          value={stats.dht ?? 'unknown'}
          title="Toggle runtime rTorrent DHT"
          state={stats.dht}
          disabled={togglingFeature === 'dht'}
          onClick={stats.dht === 'unknown' || togglingFeature ? undefined : onToggleDht}
        />
        <Badge
          label="PEX"
          value={stats.pex ?? 'unknown'}
          title="Toggle runtime rTorrent peer exchange"
          state={stats.pex}
          disabled={togglingFeature === 'pex'}
          onClick={stats.pex === 'unknown' || togglingFeature ? undefined : onTogglePex}
        />
      </div>
    </footer>
  )
}
