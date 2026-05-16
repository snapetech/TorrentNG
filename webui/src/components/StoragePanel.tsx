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
  const { data, isLoading, error } = useQuery({
    queryKey: ['storage'],
    queryFn: api.storage,
    refetchInterval: 30_000,
  })

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 12, color: '#e2e8f0' }}>
        Storage
      </div>

      {isLoading && <div style={{ color: '#64748b', fontSize: 12 }}>Loading storage stats…</div>}
      {error && <div style={{ color: '#ef4444', fontSize: 12 }}>Storage stats unavailable</div>}

      <div style={{ display: 'grid', gap: 10, maxWidth: 840 }}>
        {data?.roots.map(root => (
          <div
            key={root.path}
            style={{
              border: '1px solid #1e2433',
              borderRadius: 6,
              padding: 12,
              background: '#111827',
            }}
          >
            <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 8 }}>
              <div style={{
                flex: 1, minWidth: 0, color: root.ok ? '#cbd5e1' : '#fca5a5',
                fontSize: 13, fontFamily: 'monospace', overflow: 'hidden',
                textOverflow: 'ellipsis', whiteSpace: 'nowrap',
              }} title={root.path}>
                {root.path}
              </div>
              {root.readonly && (
                <span style={{ color: '#f59e0b', fontSize: 11 }}>read-only</span>
              )}
              <span style={{ color: root.ok ? '#94a3b8' : '#ef4444', fontSize: 12 }}>
                {root.ok ? `${root.used_percent.toFixed(1)}% used` : 'unavailable'}
              </span>
            </div>

            {root.ok ? (
              <>
                <div style={{ height: 6, background: '#1e2433', borderRadius: 3, overflow: 'hidden', marginBottom: 8 }}>
                  <div style={{
                    width: `${Math.min(100, root.used_percent)}%`,
                    height: '100%',
                    background: root.used_percent >= 90 ? '#ef4444' : root.used_percent >= 75 ? '#f59e0b' : '#22c55e',
                  }} />
                </div>
                <div style={{ display: 'flex', gap: 18, flexWrap: 'wrap', fontSize: 12, color: '#64748b' }}>
                  <span>Used {fmtBytes(root.used_bytes)}</span>
                  <span>Free {fmtBytes(root.available_bytes)}</span>
                  <span>Total {fmtBytes(root.total_bytes)}</span>
                </div>
              </>
            ) : (
              <div style={{ color: '#ef4444', fontSize: 12 }}>{root.error}</div>
            )}
          </div>
        ))}
      </div>
    </section>
  )
}
