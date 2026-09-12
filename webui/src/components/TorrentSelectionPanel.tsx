import type { TorrentSummary } from '../api/client'
import { DetailDockControls, type DetailPosition } from './DetailDockControls'

interface Props {
  torrents: TorrentSummary[]
  selectedCount: number
  position: DetailPosition
  onPositionChange: (position: DetailPosition) => void
  onClose: () => void
  onStart: () => void
  onStop: () => void
  onRecheck: () => void
  onReannounce: () => void
  onEdit: () => void
  onSequential: () => void
  onClearSelection: () => void
  busy: boolean
}

type Status = { label: string; color: string }

function fmtSize(bytes: number): string {
  if (bytes >= 1e12) return `${(bytes / 1e12).toFixed(2)} TB`
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(2)} GB`
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(1)} MB`
  if (bytes >= 1e3) return `${(bytes / 1e3).toFixed(0)} KB`
  return `${Math.max(0, Math.round(bytes))} B`
}

function fmtSpeed(bps: number): string {
  return bps > 0 ? `${fmtSize(bps)}/s` : '0 B/s'
}

function statusFor(t: TorrentSummary): Status {
  if (t.state === 3) return { label: 'Error', color: 'var(--danger)' }
  if (t.state === 0) return { label: 'Stopped', color: 'var(--faint)' }
  if (t.state === 2) return { label: 'Checking', color: 'var(--warning)' }
  if (t.state === 4) return { label: 'Metadata', color: 'var(--muted)' }
  if (t.state === 5) return { label: 'Queued', color: 'var(--muted)' }
  if (t.complete && t.is_active) return { label: 'Seeding', color: 'var(--success)' }
  if (!t.complete && t.is_active) return { label: 'Downloading', color: 'var(--accent)' }
  if (t.is_open) return { label: 'Stalled', color: 'var(--warning)' }
  return { label: 'Queued', color: 'var(--muted)' }
}

function uniqueValues(values: string[]): string[] {
  return Array.from(new Set(values.filter(Boolean)))
}

function uniformValue(values: string[]): string {
  const unique = uniqueValues(values)
  return unique.length === 1 ? unique[0] : unique.length === 0 ? 'None' : 'Mixed'
}

function ActionButton({
  label, icon, color, onClick, disabled,
}: { label: string; icon: string; color: string; onClick: () => void; disabled: boolean }) {
  return (
    <button
      type="button"
      className="tng-selection-action"
      onClick={onClick}
      disabled={disabled}
      style={{
        color: `color-mix(in srgb, ${color} 82%, var(--text))`,
        borderColor: `color-mix(in srgb, ${color} 42%, var(--border))`,
      }}
    >
      <span aria-hidden="true">{icon}</span>
      <span>{label}</span>
    </button>
  )
}

export function TorrentSelectionPanel({
  torrents, selectedCount, position, onPositionChange, onClose,
  onStart, onStop, onRecheck, onReannounce, onEdit, onSequential, onClearSelection, busy,
}: Props) {
  const knownCount = torrents.length
  const totalSize = torrents.reduce((sum, torrent) => sum + Math.max(0, torrent.size_bytes), 0)
  const totalDone = torrents.reduce((sum, torrent) => sum + Math.max(0, torrent.bytes_done), 0)
  const totalDown = torrents.reduce((sum, torrent) => sum + Math.max(0, torrent.down_rate), 0)
  const totalUp = torrents.reduce((sum, torrent) => sum + Math.max(0, torrent.up_rate), 0)
  const totalUploaded = torrents.reduce((sum, torrent) => sum + Math.max(0, torrent.up_total), 0)
  const totalDownloaded = torrents.reduce((sum, torrent) => sum + Math.max(0, torrent.down_total), 0)
  const progress = totalSize > 0 ? Math.min(100, (totalDone / totalSize) * 100) : 0
  const ratio = totalDownloaded > 0 ? totalUploaded / totalDownloaded : 0
  const statuses = Array.from(torrents.reduce((counts, torrent) => {
    const status = statusFor(torrent)
    counts.set(status.label, { status, count: (counts.get(status.label)?.count ?? 0) + 1 })
    return counts
  }, new Map<string, { status: Status; count: number }>()).values())
  const names = torrents.slice(0, 4).map(torrent => torrent.name)
  const category = uniformValue(torrents.map(torrent => torrent.category))
  const location = uniformValue(torrents.map(torrent => torrent.directory || torrent.base_path))
  const tags = uniqueValues(torrents.flatMap(torrent => torrent.tags.split(',').map(tag => tag.trim())))

  return (
    <aside className="torrent-detail torrent-selection-panel" data-position={position} aria-labelledby="selection-workspace-title" style={{
      width: 340, background: 'var(--bg)', display: 'flex', flexDirection: 'column', flexShrink: 0, minHeight: 0,
      fontSize: 12,
    }}>
      <header className="tng-selection-header" style={{
        padding: '10px 14px', borderBottom: '1px solid var(--border)', display: 'flex', alignItems: 'flex-start', gap: 8,
      }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <h2 id="selection-workspace-title" style={{ margin: 0, display: 'block', fontWeight: 800, fontSize: 13, color: 'var(--text)', lineHeight: 1.3 }}>
            Selection workspace
          </h2>
          <span style={{ display: 'block', color: 'var(--accent-text)', fontSize: 11, marginTop: 3, fontWeight: 700 }}>
            {selectedCount.toLocaleString()} torrents selected
          </span>
        </div>
        <DetailDockControls position={position} onChange={onPositionChange} />
        <button type="button" onClick={onClose} title="Hide details" aria-label="Hide details" style={{
          background: 'var(--surface-2)', border: '1px solid var(--border-strong)', borderRadius: 5,
          cursor: 'pointer', color: 'var(--muted)', fontSize: 16, lineHeight: 1, width: 28, height: 28, padding: 0, flexShrink: 0,
        }}>×</button>
      </header>

      <div className="tng-selection-body" style={{ flex: 1, minHeight: 0, overflowY: 'auto', padding: '14px 14px 0' }}>
        {knownCount < selectedCount && (
          <div role="status" style={{
            border: '1px solid color-mix(in srgb, var(--warning) 42%, var(--border))',
            background: 'color-mix(in srgb, var(--warning) 8%, var(--surface))',
            color: 'var(--warning)', borderRadius: 6, padding: '8px 9px', marginBottom: 12, fontSize: 11,
          }}>
            Summary data is available for {knownCount.toLocaleString()} of {selectedCount.toLocaleString()} selected torrents.
          </div>
        )}

        <section className="tng-selection-section" aria-labelledby="selection-summary-heading">
          <h3 id="selection-summary-heading" className="tng-selection-section-title">Aggregate view</h3>
          <div className="tng-selection-stats">
            <Stat label="Total size" value={knownCount ? fmtSize(totalSize) : '—'} />
            <Stat label="Downloaded" value={knownCount ? fmtSize(totalDownloaded) : '—'} />
            <Stat label="Progress" value={knownCount ? `${progress.toFixed(1)}%` : '—'} />
            <Stat label="Down speed" value={knownCount ? fmtSpeed(totalDown) : '—'} tone="accent" />
            <Stat label="Up speed" value={knownCount ? fmtSpeed(totalUp) : '—'} tone="success" />
            <Stat label="Ratio" value={knownCount ? ratio.toFixed(3) : '—'} />
            <Stat label="Uploaded" value={knownCount ? fmtSize(totalUploaded) : '—'} />
          </div>
          <div className="tng-selection-progress" role="progressbar" aria-label={knownCount ? `${progress.toFixed(1)} percent complete` : 'Progress unavailable'} aria-valuemin={0} aria-valuemax={100} aria-valuenow={knownCount ? progress : 0}>
            <span style={{ width: `${progress}%` }} />
          </div>
        </section>

        <section className="tng-selection-section" aria-labelledby="selection-state-heading">
          <h3 id="selection-state-heading" className="tng-selection-section-title">State mix</h3>
          <div className="tng-selection-statuses">
            {statuses.length > 0 ? statuses.map(({ status, count }) => (
              <span key={status.label} className="tng-selection-status" style={{
                color: status.color, borderColor: `color-mix(in srgb, ${status.color} 40%, var(--border))`,
                background: `color-mix(in srgb, ${status.color} 10%, transparent)`,
              }}>
                {status.label} <strong>{count.toLocaleString()}</strong>
              </span>
            )) : <span style={{ color: 'var(--faint)', fontSize: 11 }}>No summary data loaded.</span>}
          </div>
        </section>

        <section className="tng-selection-section" aria-labelledby="selection-values-heading">
          <h3 id="selection-values-heading" className="tng-selection-section-title">Shared values</h3>
          <div className="tng-selection-values">
            <ValueRow label="Category" value={category} />
            <ValueRow label="Save path" value={location} mono />
            <ValueRow label="Tags" value={tags.length ? tags.join(', ') : 'None'} />
          </div>
        </section>

        <section className="tng-selection-section" aria-labelledby="selection-actions-heading">
          <h3 id="selection-actions-heading" className="tng-selection-section-title">Bulk actions</h3>
          <div className="tng-selection-action-group-label">Transfer</div>
          <div className="tng-selection-actions">
            <ActionButton label="Start" icon="▶" color="var(--success)" onClick={onStart} disabled={busy} />
            <ActionButton label="Stop" icon="■" color="var(--muted)" onClick={onStop} disabled={busy} />
            <ActionButton label="Recheck" icon="↻" color="var(--warning)" onClick={onRecheck} disabled={busy} />
            <ActionButton label="Announce" icon="⇄" color="var(--accent)" onClick={onReannounce} disabled={busy} />
          </div>
          <div className="tng-selection-action-group-label">Manage</div>
          <div className="tng-selection-actions">
            <ActionButton label="Edit selected" icon="✎" color="var(--accent)" onClick={onEdit} disabled={busy} />
            <ActionButton label="Sequential" icon="≡" color="var(--warning)" onClick={onSequential} disabled={busy} />
          </div>
        </section>

        {names.length > 0 && (
          <section className="tng-selection-section" aria-labelledby="selection-included-heading">
            <h3 id="selection-included-heading" className="tng-selection-section-title">Included</h3>
            <div className="tng-selection-names">
              {names.map(name => <span key={name} title={name}>{name}</span>)}
              {selectedCount > names.length && <span className="tng-selection-more">+ {(selectedCount - names.length).toLocaleString()} more</span>}
            </div>
          </section>
        )}

        <div className="tng-selection-footer">
          <span>Use Edit selected for category, tags, location, and share limits.</span>
          <button type="button" onClick={onClearSelection} disabled={busy}>Clear selection</button>
        </div>
        <div style={{ height: 14 }} />
      </div>
    </aside>
  )
}

function Stat({ label, value, tone }: { label: string; value: string; tone?: 'accent' | 'success' }) {
  return (
    <div className="tng-selection-stat">
      <span>{label}</span>
      <strong data-tone={tone}>{value}</strong>
    </div>
  )
}

function ValueRow({ label, value, mono = false }: { label: string; value: string; mono?: boolean }) {
  return (
    <div className="tng-selection-value-row">
      <span>{label}</span>
      <strong className={mono ? 'tng-selection-mono' : undefined} title={value}>{value}</strong>
    </div>
  )
}
