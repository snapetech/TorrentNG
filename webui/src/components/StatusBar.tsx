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
  if (value === 'open' || value === 'on') return themedTone('var(--success)')
  if (value === 'listening' || value === 'unknown') return themedTone('var(--warning)')
  if (value === 'closed' || value === 'off') return themedTone('var(--danger)')
  return themedTone('var(--muted)')
}

function themedTone(color: string): { color: string; bg: string; border: string } {
  return {
    color,
    bg: `color-mix(in srgb, ${color} 12%, transparent)`,
    border: `color-mix(in srgb, ${color} 38%, var(--border))`,
  }
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

function Notice({ tone, children }: { tone: 'ok' | 'error'; children: React.ReactNode }) {
  return (
    <span style={{
      display: 'inline-flex', alignItems: 'center', gap: 5, minHeight: 22,
      border: '1px solid ' + (tone === 'error' ? 'color-mix(in srgb, var(--danger) 45%, var(--border))' : 'color-mix(in srgb, var(--success) 38%, var(--border))'),
      background: tone === 'error' ? 'color-mix(in srgb, var(--danger) 12%, transparent)' : 'color-mix(in srgb, var(--success) 10%, transparent)',
      color: tone === 'error' ? 'var(--danger)' : 'var(--success)',
      borderRadius: 6,
      padding: '0 8px',
      whiteSpace: 'nowrap',
    }}>
      <span>{tone === 'error' ? '!' : '✓'}</span>
      <span>{children}</span>
    </span>
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
        <Badge label="Torrents" value={total.toLocaleString()} state="idle" />
        {rendered !== total && <Badge label="Rendered" value={rendered.toLocaleString()} state="idle" />}
        {cached !== undefined && <Badge label="Cached" value={cached.toLocaleString()} state="idle" />}
        {selected > 0 && <Badge label="Selected" value={selected.toLocaleString()} state="on" />}
        {featureError && <Notice tone="error">{featureError}</Notice>}
        {actionMessage && <Notice tone={actionTone}>{actionMessage}</Notice>}
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
          title="Toggle runtime rTorrent DHT"
          state={stats.dht}
          disabled={togglingFeature === 'dht'}
          value={togglingFeature === 'dht' ? '...' : (stats.dht ?? 'unknown')}
          onClick={stats.dht === 'unknown' || togglingFeature ? undefined : onToggleDht}
        />
        <Badge
          label="PEX"
          title="Toggle runtime rTorrent peer exchange"
          state={stats.pex}
          disabled={togglingFeature === 'pex'}
          value={togglingFeature === 'pex' ? '...' : (stats.pex ?? 'unknown')}
          onClick={stats.pex === 'unknown' || togglingFeature ? undefined : onTogglePex}
        />
      </div>
    </footer>
  )
}
