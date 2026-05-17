import { useMemo, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import { api, type AppLogEvent } from '../api/client'

const LIMIT = 100

export function LogsPanel() {
  const [level, setLevel] = useState('')
  const [kind, setKind] = useState('')
  const [lastKnownId, setLastKnownId] = useState<number | undefined>()
  const query = useQuery({
    queryKey: ['logs', level, kind, lastKnownId],
    queryFn: () => api.logs({ limit: LIMIT, level, kind, last_known_id: lastKnownId }),
    refetchInterval: lastKnownId ? 5_000 : 15_000,
  })
  const logs = query.data?.logs ?? []
  const newestId = useMemo(
    () => logs.reduce((max, event) => Math.max(max, event.event_id ?? 0), lastKnownId ?? 0),
    [logs, lastKnownId],
  )

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12, flexWrap: 'wrap' }}>
        <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)', flex: 1, minWidth: 180 }}>
          Operator Logs
        </div>
        <select value={level} onChange={event => setLevel(event.target.value)} style={selectStyle} aria-label="Log level">
          <option value="">All levels</option>
          <option value="info">Info</option>
          <option value="warn">Warnings</option>
          <option value="error">Errors</option>
        </select>
        <select value={kind} onChange={event => setKind(event.target.value)} style={selectStyle} aria-label="Log kind">
          <option value="">All kinds</option>
          <option value="rtorrent_log">rTorrent</option>
          <option value="rtorrent_log_ingest_error">rTorrent ingest errors</option>
          <option value="rtorrent_log_ingest_recovered">rTorrent ingest recovery</option>
          <option value="rtorrent_sync_error">Sync errors</option>
          <option value="rtorrent_sync_recovered">Sync recovery</option>
          <option value="workflow_run">Workflows</option>
          <option value="settings_changed">Settings</option>
          <option value="admin_restart_requested">Restart requests</option>
          <option value="sidecar_started">Startup</option>
        </select>
        <button onClick={() => query.refetch()} disabled={query.isFetching} style={buttonStyle}>
          {query.isFetching ? 'Refreshing...' : 'Refresh'}
        </button>
        <button
          onClick={() => setLastKnownId(newestId || undefined)}
          disabled={!newestId || query.isFetching}
          style={buttonStyle}
        >
          Newer
        </button>
        {lastKnownId && (
          <button onClick={() => setLastKnownId(undefined)} style={buttonStyle}>
            Latest page
          </button>
        )}
      </div>

      {query.isLoading && <SkeletonRows />}
      {query.error && <Notice>Logs unavailable</Notice>}
      {!query.isLoading && !query.error && logs.length === 0 && (
        <EmptyState>No log events matched the current filters.</EmptyState>
      )}
      <div style={{ display: 'grid', gap: 8, maxWidth: 1040 }}>
        {logs.map(event => (
          <LogRow key={`${event.event_id ?? event.occurred_at}-${event.kind}`} event={event} />
        ))}
      </div>
    </section>
  )
}

function LogRow({ event }: { event: AppLogEvent }) {
  const level = normalizeLevel(event.level)
  const color = level === 'error' ? 'var(--danger)' : level === 'warn' ? 'var(--warning)' : 'var(--accent)'
  const payload = parsePayload(event.payload)
  return (
    <article style={{
      display: 'grid',
      gridTemplateColumns: '86px minmax(120px, 180px) 1fr',
      gap: 10,
      alignItems: 'start',
      border: `1px solid color-mix(in srgb, ${color} 40%, var(--border))`,
      borderRadius: 7,
      background: level === 'info' ? 'var(--surface)' : `color-mix(in srgb, ${color} 7%, var(--surface))`,
      padding: '9px 11px',
      minWidth: 0,
    }}>
      <div style={{ display: 'grid', gap: 4 }}>
        <span style={{
          width: 'fit-content',
          color,
          border: `1px solid color-mix(in srgb, ${color} 55%, var(--border))`,
          borderRadius: 999,
          padding: '1px 7px',
          fontSize: 11,
          fontWeight: 800,
          textTransform: 'uppercase',
        }}>
          {level}
        </span>
        <span style={{ color: 'var(--faint)', fontSize: 11 }}>{event.event_id ?? '-'}</span>
      </div>
      <div style={{ minWidth: 0 }}>
        <div style={{ color: 'var(--text)', fontWeight: 800, fontSize: 12, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={event.kind}>
          {event.kind}
        </div>
        <div style={{ color: 'var(--faint)', fontSize: 11 }}>{formatTime(event.occurred_at)}</div>
      </div>
      <div style={{ minWidth: 0 }}>
        <div style={{ color: 'var(--text)', fontSize: 12, lineHeight: 1.4, overflowWrap: 'anywhere' }}>
          {event.message || event.kind}
        </div>
        {payload && (
          <div style={{
            marginTop: 5,
            color: 'var(--faint)',
            fontFamily: 'monospace',
            fontSize: 11,
            overflow: 'hidden',
            textOverflow: 'ellipsis',
            whiteSpace: 'nowrap',
          }} title={payload}>
            {payload}
          </div>
        )}
      </div>
    </article>
  )
}

function normalizeLevel(level: string): 'info' | 'warn' | 'error' {
  const value = level.toLowerCase()
  if (value === 'error' || value === 'critical') return 'error'
  if (value === 'warn' || value === 'warning') return 'warn'
  return 'info'
}

function formatTime(timestamp: number): string {
  if (!timestamp) return 'unknown'
  return new Date(timestamp * 1000).toLocaleString()
}

function parsePayload(payload: string): string | null {
  try {
    const value = JSON.parse(payload) as Record<string, unknown>
    const parts = ['component', 'operation', 'result', 'source', 'torrent', 'job_id', 'tracker', 'error']
      .flatMap(key => value[key] ? [`${key}=${String(value[key])}`] : [])
    return parts.length > 0 ? parts.join(' ') : null
  } catch {
    return null
  }
}

function SkeletonRows() {
  return (
    <div style={{ display: 'grid', gap: 8, maxWidth: 1040 }}>
      {Array.from({ length: 5 }).map((_, index) => (
        <div key={index} style={{
          height: 58,
          borderRadius: 7,
          border: '1px solid var(--border)',
          background: 'linear-gradient(90deg, var(--surface), var(--surface-2), var(--surface))',
        }} />
      ))}
    </div>
  )
}

function EmptyState({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--faint)',
      fontSize: 12,
      border: '1px dashed var(--border-strong)',
      borderRadius: 7,
      background: 'color-mix(in srgb, var(--surface) 72%, transparent)',
      padding: 14,
      maxWidth: 1040,
    }}>
      {children}
    </div>
  )
}

function Notice({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--danger)',
      border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
      background: 'color-mix(in srgb, var(--danger) 8%, var(--surface))',
      borderRadius: 7,
      padding: 10,
      fontSize: 12,
      maxWidth: 1040,
    }}>
      {children}
    </div>
  )
}

const selectStyle: React.CSSProperties = {
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '5px 8px',
  fontSize: 12,
}

const buttonStyle: React.CSSProperties = {
  background: 'none',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--muted)',
  padding: '5px 9px',
  fontSize: 12,
  cursor: 'pointer',
}
