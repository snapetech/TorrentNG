import { useQuery } from '@tanstack/react-query'
import { api } from '../api/client'

function fmtBytes(bytes: number): string {
  if (bytes >= 1e12) return (bytes / 1e12).toFixed(2) + ' TB'
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(2) + ' GB'
  if (bytes >= 1e6) return (bytes / 1e6).toFixed(1) + ' MB'
  if (bytes >= 1e3) return (bytes / 1e3).toFixed(0) + ' KB'
  return bytes + ' B'
}

export function StoragePanel() {
  const { data, isLoading, isFetching, error, refetch } = useQuery({
    queryKey: ['storage'],
    queryFn: api.storage,
    staleTime: 5_000,
    refetchInterval: 10_000,
  })

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12 }}>
        <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text)', flex: 1 }}>
          Storage
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

      {isLoading && <SkeletonRows rows={2} />}
      {error && <Notice>Storage stats unavailable</Notice>}

      <div style={{ display: 'grid', gap: 10, maxWidth: 840 }}>
        {data && data.roots.length === 0 && (
          <EmptyState>No storage roots reported.</EmptyState>
        )}
        {data?.roots.map(root => (
          <StorageRootCard
            key={root.path}
            root={root}
          />
        ))}
      </div>
    </section>
  )
}

function StorageRootCard({ root }: { root: NonNullable<Awaited<ReturnType<typeof api.storage>>['roots']>[number] }) {
  const tone = !root.ok
    ? 'var(--danger)'
    : root.used_percent >= 90
      ? 'var(--danger)'
      : root.used_percent >= 75
        ? 'var(--warning)'
        : 'var(--success)'
  return (
    <div
      className="rtng-card rtng-storage-root"
      data-tone={!root.ok ? 'error' : root.used_percent >= 90 ? 'error' : root.used_percent >= 75 ? 'warn' : 'ok'}
      style={{
        border: '1px solid var(--border)',
        borderRadius: 7,
        padding: 12,
        background: 'var(--surface)',
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 8 }}>
        <div style={{
          flex: 1, minWidth: 0, color: root.ok ? 'var(--text)' : 'var(--danger)',
          fontSize: 13, fontFamily: 'monospace', overflow: 'hidden',
          textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }} title={root.path}>
          {root.path}
        </div>
        {root.readonly && (
          <span style={{
            color: 'var(--warning)', border: '1px solid color-mix(in srgb, var(--warning) 45%, var(--border))',
            background: 'color-mix(in srgb, var(--warning) 9%, transparent)', borderRadius: 999,
            padding: '1px 7px', fontSize: 11, fontWeight: 700,
          }}>read-only</span>
        )}
        <span style={{ color: tone, fontSize: 12, fontWeight: 700 }}>
          {root.ok ? `${root.used_percent.toFixed(1)}% used` : 'unavailable'}
        </span>
      </div>

      {root.ok ? (
        <>
          <div className="rtng-storage-meter" style={{ height: 8, background: 'var(--surface-2)', borderRadius: 99, overflow: 'hidden', marginBottom: 9 }}>
            <div style={{ width: `${Math.min(100, root.used_percent)}%`, height: '100%', background: tone }} />
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(120px, 1fr))', gap: 8, fontSize: 12 }}>
            <StorageMetric label="Used" value={fmtBytes(root.used_bytes)} />
            <StorageMetric label="Free" value={fmtBytes(root.available_bytes)} />
            <StorageMetric label="Total" value={fmtBytes(root.total_bytes)} />
          </div>
        </>
      ) : (
        <div style={{ color: 'var(--danger)', fontSize: 12 }}>{root.error}</div>
      )}
    </div>
  )
}

function SkeletonRows({ rows }: { rows: number }) {
  return (
    <div style={{ display: 'grid', gap: 10, maxWidth: 840 }}>
      {Array.from({ length: rows }).map((_, index) => (
        <div key={index} style={{
          border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)', padding: 12,
          display: 'grid', gap: 9,
        }}>
          <span className="rtng-skeleton" style={{ width: '55%', height: 12 }} />
          <span className="rtng-skeleton" style={{ width: '100%', height: 8 }} />
          <span className="rtng-skeleton" style={{ width: '72%', height: 24 }} />
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

function EmptyState({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--faint)', fontSize: 12, border: '1px dashed var(--border-strong)',
      borderRadius: 7, background: 'color-mix(in srgb, var(--surface) 72%, transparent)', padding: 14,
    }}>{children}</div>
  )
}

function StorageMetric({ label, value }: { label: string; value: string }) {
  return (
    <span className="rtng-metric-tile" style={{
      display: 'grid', gap: 2, border: '1px solid var(--border)', borderRadius: 6,
      background: 'var(--bg)', padding: '6px 8px', minWidth: 0,
    }}>
      <span style={{ color: 'var(--faint)', fontSize: 10, textTransform: 'uppercase', fontWeight: 700 }}>{label}</span>
      <span style={{ color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{value}</span>
    </span>
  )
}
