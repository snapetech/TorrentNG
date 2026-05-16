import { useState, useCallback, useEffect } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useTorrentsInfinite, flattenPages, useHealth } from './hooks/useTorrents'
import { useWebSocket } from './hooks/useWebSocket'
import { TorrentTable } from './components/TorrentTable'
import { FilterBar } from './components/FilterBar'
import { TorrentDetail } from './components/TorrentDetail'
import { BulkActionBar } from './components/BulkActionBar'
import { AddTorrentDialog } from './components/AddTorrentDialog'
import { UserAgentPanel } from './components/UserAgentPanel'
import { CategoriesPanel } from './components/CategoriesPanel'
import { SavedViewsBar } from './components/SavedViewsBar'
import { StoragePanel } from './components/StoragePanel'
import { TrackerHealthPanel } from './components/TrackerHealthPanel'
import { RatioGroupsPanel } from './components/RatioGroupsPanel'
import { WorkflowsPanel } from './components/WorkflowsPanel'
import { RssRulesPanel } from './components/RssRulesPanel'
import { EnginePanel } from './components/EnginePanel'
import { api, AuthError, type ListParams, type TorrentSummary } from './api/client'

type View = 'torrents' | 'settings'
type AuthState = 'checking' | 'authenticated' | 'unauthenticated'

function fmtSpeed(bps: number): string {
  if (!bps) return '0 B/s'
  if (bps >= 1e9) return (bps / 1e9).toFixed(1) + ' GB/s'
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' MB/s'
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' KB/s'
  return bps + ' B/s'
}

export function App() {
  const qc = useQueryClient()
  const [authState, setAuthState] = useState<AuthState>('checking')
  const [authMessage, setAuthMessage] = useState('')
  const [view, setView] = useState<View>('torrents')
  const [params, setParams] = useState<Omit<ListParams, 'limit' | 'offset'>>({
    sort: 'name',
    dir: 'asc',
  })
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [detailHash, setDetailHash] = useState<string | null>(null)
  const [speeds, setSpeeds] = useState({ up: 0, dn: 0 })
  const [addOpen, setAddOpen] = useState(false)

  const isAuthed = authState === 'authenticated'
  const query = useTorrentsInfinite(params, isAuthed)
  const { torrents, total } = flattenPages(query.data)
  const { data: health } = useHealth()

  const handleStats = useCallback((up: number, dn: number) => setSpeeds({ up, dn }), [])
  useWebSocket(handleStats, isAuthed)

  useEffect(() => {
    let cancelled = false
    api.auth.check()
      .then(() => {
        if (!cancelled) setAuthState('authenticated')
      })
      .catch((err) => {
        if (cancelled) return
        setAuthState('unauthenticated')
        if (!(err instanceof AuthError)) {
          setAuthMessage('Could not reach the API.')
        }
      })
    return () => { cancelled = true }
  }, [])

  useEffect(() => {
    if (query.error instanceof AuthError) {
      setAuthState('unauthenticated')
      setSelected(new Set())
      setDetailHash(null)
    }
  }, [query.error])

  // Keyboard shortcuts
  useEffect(() => {
    function onKey(e: KeyboardEvent) {
      if (e.key === 'Escape') {
        if (addOpen) { setAddOpen(false); return }
        if (detailHash) { setDetailHash(null); return }
        if (selected.size > 0) { setSelected(new Set()); return }
      }
      // 'a' key to open add dialog when not in an input
      if (e.key === 'a' && !(e.target instanceof HTMLInputElement) && !(e.target instanceof HTMLTextAreaElement)) {
        setAddOpen(true)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [addOpen, detailHash, selected.size])

  function updateParams(p: Partial<typeof params>) {
    setParams(prev => ({ ...prev, ...p }))
  }

  function applySavedView(next: typeof params) {
    setParams({
      sort: 'name',
      dir: 'asc',
      ...next,
    })
    setSelected(new Set())
  }

  function handleSelect(hash: string) {
    setSelected(prev => {
      const next = new Set(prev)
      if (next.has(hash)) next.delete(hash)
      else next.add(hash)
      return next
    })
  }

  function handleSelectAll(hashes: string[]) {
    setSelected(new Set(hashes))
  }

  function handleSort(sortKey: string) {
    setParams(prev => ({
      ...prev,
      sort: sortKey,
      dir: prev.sort === sortKey ? (prev.dir === 'asc' ? 'desc' : 'asc') : 'asc',
    }))
  }

  const detailTorrent: TorrentSummary | undefined =
    detailHash ? torrents.find(t => t.hash === detailHash) : undefined

  async function handleLogin(username: string, password: string) {
    setAuthMessage('')
    await api.auth.login(username, password)
    setAuthState('authenticated')
    setSelected(new Set())
    setDetailHash(null)
    await qc.invalidateQueries()
  }

  async function handleLogout() {
    await api.auth.logout()
    setAuthState('unauthenticated')
    setSelected(new Set())
    setDetailHash(null)
    setSpeeds({ up: 0, dn: 0 })
    qc.clear()
  }

  if (authState === 'checking') {
    return (
      <div style={{
        minHeight: '100vh', background: '#0d1117', color: '#64748b',
        display: 'grid', placeItems: 'center', fontSize: 13,
      }}>
        Checking session...
      </div>
    )
  }

  if (authState === 'unauthenticated') {
    return <LoginScreen message={authMessage} onLogin={handleLogin} />
  }

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
          <button onClick={() => setAddOpen(true)} title="Add torrent (A)" style={{
            background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
            color: '#93c5fd', padding: '3px 12px', fontSize: 12, cursor: 'pointer',
          }}>+ Add</button>
        )}

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

        <span style={{ fontSize: 11, color: '#3b82f6', marginLeft: 'auto' }}>
          ↓ {fmtSpeed(speeds.dn)}
        </span>
        <span style={{ fontSize: 11, color: '#22c55e' }}>
          ↑ {fmtSpeed(speeds.up)}
        </span>
        <button onClick={handleLogout} title="Log out" style={{
          background: 'transparent', border: '1px solid #334155', borderRadius: 5,
          color: '#94a3b8', padding: '3px 10px', fontSize: 12, cursor: 'pointer',
        }}>Log out</button>
      </header>

      {view === 'torrents' && (
        <FilterBar params={params} onChange={updateParams} />
      )}
      {view === 'torrents' && (
        <SavedViewsBar params={params} onApply={applySavedView} />
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
              <StoragePanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <EnginePanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <TrackerHealthPanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <RatioGroupsPanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <WorkflowsPanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <RssRulesPanel />
            </div>
            <div style={{ borderBottom: '1px solid #1e2433' }}>
              <UserAgentPanel />
            </div>
          </div>
        )}

        {view === 'torrents' && (
          <>
            <div style={{ flex: 1, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
              {query.isError && (
                <div style={{ padding: 24, color: '#ef4444', textAlign: 'center' }}>
                  Failed to connect to sidecar API.
                </div>
              )}
              {query.isLoading && !query.data && (
                <div style={{ padding: 24, color: '#64748b', textAlign: 'center' }}>Loading…</div>
              )}
              {query.data && (
                <TorrentTable
                  torrents={torrents}
                  total={total}
                  selected={selected}
                  params={params}
                  onSelect={handleSelect}
                  onSelectAll={handleSelectAll}
                  onDetail={hash => setDetailHash(hash)}
                  onSort={handleSort}
                  onLoadMore={() => query.fetchNextPage()}
                  hasMore={query.hasNextPage ?? false}
                  isFetchingMore={query.isFetchingNextPage}
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

function LoginScreen({ message, onLogin }: {
  message: string
  onLogin: (username: string, password: string) => Promise<void>
}) {
  const [username, setUsername] = useState('keith')
  const [password, setPassword] = useState('')
  const [error, setError] = useState(message)
  const [busy, setBusy] = useState(false)

  useEffect(() => setError(message), [message])

  async function submit(e: React.FormEvent) {
    e.preventDefault()
    setBusy(true)
    setError('')
    try {
      await onLogin(username.trim(), password)
    } catch (err) {
      setError(err instanceof AuthError ? err.message : 'Login failed.')
    } finally {
      setBusy(false)
    }
  }

  return (
    <div style={{
      minHeight: '100vh', background: '#0d1117', color: '#e2e8f0',
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <form onSubmit={submit} style={{
        width: 'min(360px, 100%)', border: '1px solid #1e2433', borderRadius: 8,
        background: '#0f141d', padding: 20, display: 'flex', flexDirection: 'column', gap: 12,
      }}>
        <div>
          <div style={{ fontWeight: 700, fontSize: 18 }}>rtorrentNG</div>
          <div style={{ color: '#64748b', fontSize: 12, marginTop: 4 }}>Sign in to manage torrents</div>
        </div>
        <label style={{ display: 'flex', flexDirection: 'column', gap: 5, fontSize: 12, color: '#94a3b8' }}>
          Username
          <input
            autoFocus
            value={username}
            onChange={e => setUsername(e.target.value)}
            autoComplete="username"
            style={{
              background: '#0d1117', border: '1px solid #334155', borderRadius: 5,
              color: '#e2e8f0', padding: '8px 10px', fontSize: 14,
            }}
          />
        </label>
        <label style={{ display: 'flex', flexDirection: 'column', gap: 5, fontSize: 12, color: '#94a3b8' }}>
          Password
          <input
            type="password"
            value={password}
            onChange={e => setPassword(e.target.value)}
            autoComplete="current-password"
            style={{
              background: '#0d1117', border: '1px solid #334155', borderRadius: 5,
              color: '#e2e8f0', padding: '8px 10px', fontSize: 14,
            }}
          />
        </label>
        {error && <div style={{ color: '#f87171', fontSize: 12 }}>{error}</div>}
        <button disabled={busy} style={{
          marginTop: 4, background: busy ? '#1e293b' : '#1e3a5f',
          border: '1px solid #3b82f6', borderRadius: 5, color: '#bfdbfe',
          padding: '8px 12px', fontSize: 13, cursor: busy ? 'default' : 'pointer',
        }}>
          {busy ? 'Signing in...' : 'Sign in'}
        </button>
      </form>
    </div>
  )
}
