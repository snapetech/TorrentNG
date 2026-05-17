import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'

function hostLabel(url: string): string {
  try {
    return new URL(url).host || url
  } catch {
    return url
  }
}

function fmtDate(ts: number): string {
  if (!ts) return 'never'
  return new Date(ts * 1000).toLocaleString()
}

export function TrackerHealthPanel() {
  const { data, isLoading, isFetching, error, refetch } = useQuery({
    queryKey: ['tracker-health'],
    queryFn: api.trackerHealth,
    staleTime: 5_000,
    refetchInterval: 10_000,
  })
  const totals = data?.trackers.reduce((acc, tracker) => {
    acc.torrents += tracker.torrent_count
    acc.active += tracker.active_count
    acc.errors += tracker.error_count
    return acc
  }, { torrents: 0, active: 0, errors: 0 })

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12 }}>
        <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text)', flex: 1 }}>
          Tracker Health
        </div>
        <button
          onClick={() => refetch()}
          disabled={isFetching}
          style={{
            background: 'none', border: '1px solid var(--border-strong)', borderRadius: 5,
            color: 'var(--muted)', padding: '4px 9px', fontSize: 12,
            cursor: isFetching ? 'not-allowed' : 'pointer', opacity: isFetching ? 0.55 : 1,
          }}
        >
          {isFetching ? 'Refreshing…' : 'Refresh'}
        </button>
      </div>

      {isLoading && <TrackerSkeleton />}
      {error && <Notice>Tracker health unavailable</Notice>}
      {data && data.trackers.length === 0 && (
        <div style={{
          color: 'var(--faint)', fontSize: 12, border: '1px dashed var(--border-strong)',
          borderRadius: 7, background: 'color-mix(in srgb, var(--surface) 72%, transparent)', padding: 14,
        }}>No tracker data cached yet</div>
      )}
      {totals && (
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', marginBottom: 12 }}>
          <Summary label="Trackers" value={data?.trackers.length ?? 0} tone="neutral" />
          <Summary label="Torrents" value={totals.torrents} tone="neutral" />
          <Summary label="Active" value={totals.active} tone="ok" />
          <Summary label="Errors" value={totals.errors} tone={totals.errors > 0 ? 'warn' : 'ok'} />
        </div>
      )}

      <div style={{ display: 'grid', gap: 8, maxWidth: 980, overflowX: 'auto', paddingBottom: 2 }}>
        {data?.trackers.map(tracker => {
          const errorRatio = tracker.torrent_count > 0
            ? tracker.error_count / tracker.torrent_count
            : 0
          const color = errorRatio >= 0.5 ? 'var(--danger)' : errorRatio > 0 ? 'var(--warning)' : 'var(--success)'
          return (
            <div
              key={tracker.tracker}
              className="rtng-card"
              style={{
                display: 'grid',
                gridTemplateColumns: 'minmax(220px, 1fr) 90px 90px 90px 120px',
                minWidth: 680,
                gap: 12,
                alignItems: 'center',
                border: `1px solid color-mix(in srgb, ${color} 45%, var(--border))`,
                borderRadius: 6,
                padding: '9px 12px',
                background: errorRatio > 0 ? 'color-mix(in srgb, var(--warning) 7%, var(--surface))' : 'var(--surface)',
                fontSize: 12,
              }}
            >
              <div style={{ minWidth: 0 }}>
                <div style={{ display: 'flex', alignItems: 'center', gap: 7 }}>
                  <span style={{ width: 7, height: 7, borderRadius: '50%', background: color, flexShrink: 0 }} />
                  <span style={{
                    color: 'var(--text)', fontWeight: 700, overflow: 'hidden',
                    textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }} title={tracker.tracker}>
                    {hostLabel(tracker.tracker)}
                  </span>
                </div>
                <div style={{
                  color: 'var(--faint)', fontFamily: 'monospace', overflow: 'hidden',
                  textOverflow: 'ellipsis', whiteSpace: 'nowrap', marginTop: 2,
                }} title={tracker.tracker}>
                  {tracker.tracker}
                </div>
              </div>

              <Metric label="Torrents" value={tracker.torrent_count} color="var(--muted)" />
              <Metric label="Active" value={tracker.active_count} color="var(--accent)" />
              <Metric label="Errors" value={tracker.error_count} color={color} />
              <div style={{ color: 'var(--faint)', textAlign: 'right' }}>
                <div>{tracker.seed_count} seeds</div>
                <div>{tracker.peer_count} peers</div>
                <div style={{ color: 'var(--faint)', marginTop: 2 }}>{fmtDate(tracker.last_updated)}</div>
              </div>
            </div>
          )
        })}
      </div>
    </section>
  )
}

function TrackerSkeleton() {
  return (
    <div style={{ display: 'grid', gap: 8, maxWidth: 980 }}>
      {Array.from({ length: 3 }).map((_, index) => (
        <div key={index} style={{
          border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)',
          padding: '10px 12px', display: 'grid', gap: 7,
        }}>
          <span className="rtng-skeleton" style={{ width: '45%', height: 12 }} />
          <span className="rtng-skeleton" style={{ width: '85%', height: 10 }} />
          <span className="rtng-skeleton" style={{ width: '62%', height: 18 }} />
        </div>
      ))}
    </div>
  )
}

function Notice({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--danger)', background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
      border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
      borderRadius: 6, padding: '8px 9px', fontSize: 12, marginBottom: 10,
    }}>{children}</div>
  )
}

function Summary({ label, value, tone }: { label: string; value: number; tone: 'ok' | 'warn' | 'neutral' }) {
  const color = tone === 'ok' ? 'var(--success)' : tone === 'warn' ? 'var(--warning)' : 'var(--muted)'
  return (
    <span style={{
      display: 'inline-flex', alignItems: 'center', gap: 6,
      border: '1px solid var(--border)', borderRadius: 6,
      background: 'var(--surface)', padding: '5px 8px', fontSize: 12,
    }}>
      <span style={{ color: 'var(--faint)' }}>{label}</span>
      <span style={{ color, fontWeight: 800, fontVariantNumeric: 'tabular-nums' }}>{value.toLocaleString()}</span>
    </span>
  )
}

function Metric({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div>
      <div style={{ color, fontWeight: 700, fontSize: 15 }}>{value.toLocaleString()}</div>
      <div style={{ color: 'var(--faint)', fontSize: 11 }}>{label}</div>
    </div>
  )
}
