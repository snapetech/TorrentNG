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

      {isLoading && <div style={{ color: 'var(--faint)', fontSize: 12 }}>Loading tracker health…</div>}
      {error && <div style={{ color: '#ef4444', fontSize: 12 }}>Tracker health unavailable</div>}
      {data && data.trackers.length === 0 && (
        <div style={{ color: 'var(--faint)', fontSize: 12 }}>No tracker data cached yet</div>
      )}

      <div style={{ display: 'grid', gap: 8, maxWidth: 980, overflowX: 'auto', paddingBottom: 2 }}>
        {data?.trackers.map(tracker => {
          const errorRatio = tracker.torrent_count > 0
            ? tracker.error_count / tracker.torrent_count
            : 0
          const color = errorRatio >= 0.5 ? '#ef4444' : errorRatio > 0 ? '#f59e0b' : '#22c55e'
          return (
            <div
              key={tracker.tracker}
              style={{
                display: 'grid',
                gridTemplateColumns: 'minmax(220px, 1fr) 90px 90px 90px 120px',
                minWidth: 680,
                gap: 12,
                alignItems: 'center',
                border: '1px solid var(--border)',
                borderRadius: 6,
                padding: '9px 12px',
                background: 'var(--surface)',
                fontSize: 12,
              }}
            >
              <div style={{ minWidth: 0 }}>
                <div style={{
                  color: 'var(--text)', fontWeight: 600, overflow: 'hidden',
                  textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                }} title={tracker.tracker}>
                  {hostLabel(tracker.tracker)}
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

function Metric({ label, value, color }: { label: string; value: number; color: string }) {
  return (
    <div>
      <div style={{ color, fontWeight: 700, fontSize: 15 }}>{value.toLocaleString()}</div>
      <div style={{ color: 'var(--faint)', fontSize: 11 }}>{label}</div>
    </div>
  )
}
