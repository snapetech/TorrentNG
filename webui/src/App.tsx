import { useState, useCallback } from 'react'
import { useTorrents, useHealth } from './hooks/useTorrents'
import { useWebSocket } from './hooks/useWebSocket'
import { TorrentTable } from './components/TorrentTable'
import { FilterBar } from './components/FilterBar'
import { TorrentDetail } from './components/TorrentDetail'
import { BulkActionBar } from './components/BulkActionBar'
import { AddTorrentDialog } from './components/AddTorrentDialog'
import { UserAgentPanel } from './components/UserAgentPanel'
import { CategoriesPanel } from './components/CategoriesPanel'
import type { ListParams, TorrentSummary } from './api/client'

type View = 'torrents' | 'settings'

function fmtSpeed(bps: number): string {
  if (!bps) return '0 B/s'
  if (bps >= 1e9) return (bps / 1e9).toFixed(1) + ' GB/s'
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' MB/s'
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' KB/s'
  return bps + ' B/s'
}

export function App() {
  const [view, setView] = useState<View>('torrents')
  const [params, setParams] = useState<ListParams>({
    sort: 'name',
    dir: 'asc',
    limit: 500,
    offset: 0,
  })
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [detailHash, setDetailHash] = useState<string | null>(null)
  const [speeds, setSpeeds] = useState({ up: 0, dn: 0 })
  const [addOpen, setAddOpen] = useState(false)

  const { data, isLoading, isError } = useTorrents(params)
  const { data: health } = useHealth()

  const handleStats = useCallback((up: number, dn: number) => setSpeeds({ up, dn }), [])
  useWebSocket(handleStats)

  function updateParams(p: Partial<ListParams>) {
    setParams(prev => ({ ...prev, ...p }))
  }

  function handleSelect(hash: string) {
    setSelected(prev => {
      const next = new Set(prev)
      if (next.has(hash)) next.delete(hash)
      else next.add(hash)
      return next
    })
  }

  function handleSort(sortKey: string) {
    setParams(prev => ({
      ...prev,
      sort: sortKey,
      dir: prev.sort === sortKey ? (prev.dir === 'asc' ? 'desc' : 'asc') : 'asc',
      offset: 0,
    }))
  }

  function handleDetail(hash: string | null) {
    setDetailHash(hash)
  }

  const detailTorrent: TorrentSummary | undefined =
    detailHash ? data?.torrents.find(t => t.hash === detailHash) : undefined

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100vh', background: '#0d1117', color: '#e2e8f0' }}>
      {/* Topbar */}
      <header style={{
        height: 44, background: '#0d1117', borderBottom: '1px solid #1e2433',
        display: 'flex', alignItems: 'center', padding: '0 16px', gap: 12, flexShrink: 0,
      }}>
        <span style={{ fontWeight: 700, fontSize: 15, letterSpacing: '-0.02em', color: '#e2e8f0' }}>
          rtorrentNG
        </span>

        <nav style={{ display: 'flex', gap: 4, marginLeft: 4 }}>
          {(['torrents', 'settings'] as View[]).map(v => (
            <button key={v} onClick={() => setView(v)} style={{
              background: view === v ? '#1e3a5f' : 'transparent',
              border: '1px solid ' + (view === v ? '#3b82f6' : 'transparent'),
              borderRadius: 5, color: view === v ? '#93c5fd' : '#64748b',
              padding: '2px 10px', fontSize: 12, cursor: 'pointer', textTransform: 'capitalize',
            }}>{v}</button>
          ))}
        </nav>

        {view === 'torrents' && (
          <button onClick={() => setAddOpen(true)} style={{
            background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
            color: '#93c5fd', padding: '3px 12px', fontSize: 12, cursor: 'pointer',
          }}>+ Add</button>
        )}

        {/* Health indicator */}
        <span style={{
          fontSize: 11, color: health?.rtorrent === 'connected' ? '#22c55e' : '#ef4444',
          display: 'flex', alignItems: 'center', gap: 4,
        }}>
          <span style={{
            width: 6, height: 6, borderRadius: '50%',
            background: health?.rtorrent === 'connected' ? '#22c55e' : '#ef4444',
            display: 'inline-block',
          }} />
          {health?.rtorrent ?? 'connecting…'}
        </span>

        {health && (
          <span style={{ fontSize: 11, color: '#475569' }}>
            {health.cached_torrents.toLocaleString()} cached
          </span>
        )}

        {/* Live speeds */}
        <span style={{ fontSize: 11, color: '#3b82f6', marginLeft: 'auto' }}>
          ↓ {fmtSpeed(speeds.dn)}
        </span>
        <span style={{ fontSize: 11, color: '#22c55e' }}>
          ↑ {fmtSpeed(speeds.up)}
        </span>
      </header>

      {view === 'torrents' && (
        <FilterBar params={params} onChange={updateParams} />
      )}
      {view === 'torrents' && selected.size > 0 && (
        <BulkActionBar hashes={[...selected]} onClear={() => setSelected(new Set())} />
      )}

      {/* Main content */}
      <main style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
        {view === 'settings' && (
          <div style={{ flex: 1, overflowY: 'auto', background: '#0f1117' }}>
            <div style={{ padding: '20px 24px', borderBottom: '1px solid #1e2433', fontSize: 16, fontWeight: 600 }}>
              Settings
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <CategoriesPanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <UserAgentPanel />
            </div>
          </div>
        )}

        {view === 'torrents' && (
          <>
            <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
              {isError && (
                <div style={{ padding: 24, color: '#ef4444', textAlign: 'center' }}>
                  Failed to connect to sidecar API.
                </div>
              )}
              {isLoading && !data && (
                <div style={{ padding: 24, color: '#64748b', textAlign: 'center' }}>Loading…</div>
              )}
              {data && (
                <TorrentTable
                  torrents={data.torrents}
                  total={data.total}
                  selected={selected}
                  params={params}
                  onSelect={handleSelect}
                  onDetail={handleDetail}
                  onSort={handleSort}
                  detailHash={detailHash}
                />
              )}
            </div>

            {detailTorrent && (
              <TorrentDetail
                torrent={detailTorrent}
                onClose={() => setDetailHash(null)}
              />
            )}
          </>
        )}
      </main>

      {addOpen && <AddTorrentDialog onClose={() => setAddOpen(false)} />}
    </div>
  )
}
