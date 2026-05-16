import { useState, useCallback, useEffect } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTorrentsInfinite, flattenPages, useHealth } from './hooks/useTorrents'
import { useWebSocket } from './hooks/useWebSocket'
import { TorrentTable } from './components/TorrentTable'
import { FilterBar } from './components/FilterBar'
import { TorrentDetail } from './components/TorrentDetail'
import { BulkActionBar } from './components/BulkActionBar'
import { AddTorrentDialog } from './components/AddTorrentDialog'
import { UserAgentPanel } from './components/UserAgentPanel'
import { CategoriesPanel } from './components/CategoriesPanel'
import { TorrentSidebar } from './components/TorrentSidebar'
import { StoragePanel } from './components/StoragePanel'
import { TrackerHealthPanel } from './components/TrackerHealthPanel'
import { RatioGroupsPanel } from './components/RatioGroupsPanel'
import { WorkflowsPanel } from './components/WorkflowsPanel'
import { RssRulesPanel } from './components/RssRulesPanel'
import { EnginePanel } from './components/EnginePanel'
import { TorrentToolbar } from './components/TorrentToolbar'
import { TorrentContextMenu, type ContextMenuState } from './components/TorrentContextMenu'
import { HelpDialog } from './components/HelpDialog'
import { StatusBar } from './components/StatusBar'
import { TorrentPropertiesDialog } from './components/TorrentPropertiesDialog'
import { AppearancePanel, type MediaInferenceMode } from './components/AppearancePanel'
import { BulkEditDialog } from './components/BulkEditDialog'
import { api, AuthError, type ListParams, type LiveStats, type TorrentSummary } from './api/client'

type View = 'torrents' | 'settings'
type AuthState = 'checking' | 'authenticated' | 'unauthenticated'
type SettingsSection = 'library' | 'engine' | 'automation' | 'support'
const MEDIA_INFERENCE_KEY = 'rtng.mediaInference'
const ACTIVE_TAB_KEY = 'rtng.activeTab'
const ACTIVE_TAB_TTL_MS = 8000

function loadMediaInference(): MediaInferenceMode {
  let value: string | null = null
  try {
    value = localStorage.getItem(MEDIA_INFERENCE_KEY)
  } catch {
    value = null
  }
  return value === 'full' || value === 'suffix' || value === 'hints' || value === 'off' ? value : 'full'
}

function fmtSpeed(bps: number): string {
  if (!bps) return '0 B/s'
  if (bps >= 1e9) return (bps / 1e9).toFixed(1) + ' GB/s'
  if (bps >= 1e6) return (bps / 1e6).toFixed(1) + ' MB/s'
  if (bps >= 1e3) return (bps / 1e3).toFixed(0) + ' KB/s'
  return bps + ' B/s'
}

function makeTabId() {
  try {
    return crypto.randomUUID()
  } catch {
    return `${Date.now()}-${Math.random()}`
  }
}

function readActiveOwner(): { id: string; ts: number } | null {
  try {
    const raw = localStorage.getItem(ACTIVE_TAB_KEY)
    if (!raw) return null
    const parsed = JSON.parse(raw) as { id?: unknown; ts?: unknown }
    return typeof parsed.id === 'string' && typeof parsed.ts === 'number'
      ? { id: parsed.id, ts: parsed.ts }
      : null
  } catch {
    return null
  }
}

function writeActiveOwner(id: string) {
  localStorage.setItem(ACTIVE_TAB_KEY, JSON.stringify({ id, ts: Date.now() }))
}

function useSingleActiveTab() {
  const [tabId] = useState(makeTabId)
  const [isActive, setIsActive] = useState(true)

  const claim = useCallback((force = false) => {
    try {
      const now = Date.now()
      const owner = readActiveOwner()
      const ownerExpired = !owner || now - owner.ts > ACTIVE_TAB_TTL_MS
      if (force || ownerExpired || owner.id === tabId) {
        writeActiveOwner(tabId)
        setIsActive(true)
      } else {
        setIsActive(false)
      }
    } catch {
      setIsActive(true)
    }
  }, [tabId])

  useEffect(() => {
    claim(false)
    const interval = window.setInterval(() => claim(false), 3000)
    function onStorage(e: StorageEvent) {
      if (e.key === ACTIVE_TAB_KEY) claim(false)
    }
    function onVisibility() {
      if (!document.hidden) claim(false)
    }
    window.addEventListener('storage', onStorage)
    document.addEventListener('visibilitychange', onVisibility)
    return () => {
      window.clearInterval(interval)
      window.removeEventListener('storage', onStorage)
      document.removeEventListener('visibilitychange', onVisibility)
      try {
        if (readActiveOwner()?.id === tabId) localStorage.removeItem(ACTIVE_TAB_KEY)
      } catch {
        // localStorage can be disabled by browser policy.
      }
    }
  }, [claim, tabId])

  return {
    isActive,
    takeOver: () => claim(true),
  }
}

export function App() {
  const qc = useQueryClient()
  const activeTab = useSingleActiveTab()
  const [authState, setAuthState] = useState<AuthState>('checking')
  const [authMessage, setAuthMessage] = useState('')
  const [view, setView] = useState<View>('torrents')
  const [params, setParams] = useState<Omit<ListParams, 'limit' | 'offset'>>({
    sort: 'name',
    dir: 'asc',
  })
  const [selected, setSelected] = useState<Set<string>>(new Set())
  const [detailHash, setDetailHash] = useState<string | null>(null)
  const [liveStats, setLiveStats] = useState<LiveStats>({ upload_speed: 0, download_speed: 0 })
  const [addOpen, setAddOpen] = useState(false)
  const [helpOpen, setHelpOpen] = useState(false)
  const [toolbarBusy, setToolbarBusy] = useState(false)
  const [contextMenu, setContextMenu] = useState<ContextMenuState | null>(null)
  const [pendingDelete, setPendingDelete] = useState<TorrentSummary | null>(null)
  const [propertiesHash, setPropertiesHash] = useState<string | null>(null)
  const [bulkEditOpen, setBulkEditOpen] = useState(false)
  const [settingsSection, setSettingsSection] = useState<SettingsSection>('library')
  const [mediaInference, setMediaInference] = useState<MediaInferenceMode>(loadMediaInference)

  const isAuthed = activeTab.isActive && authState === 'authenticated'
  const query = useTorrentsInfinite(params, isAuthed)
  const { torrents, total } = flattenPages(query.data)
  const { data: health } = useHealth(activeTab.isActive && authState === 'authenticated')
  const { data: storage } = useQuery({
    queryKey: ['storage', 'status-bar'],
    queryFn: api.storage,
    enabled: isAuthed,
    refetchInterval: 30_000,
  })

  const handleStats = useCallback((stats: LiveStats) => setLiveStats(stats), [])
  useWebSocket(handleStats, isAuthed)

  useEffect(() => {
    if (!activeTab.isActive) return
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
  }, [activeTab.isActive])

  useEffect(() => {
    if (activeTab.isActive) return
    qc.clear()
    setLiveStats({ upload_speed: 0, download_speed: 0 })
  }, [activeTab.isActive, qc])

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
        if (contextMenu) { setContextMenu(null); return }
        if (helpOpen) { setHelpOpen(false); return }
        if (pendingDelete) { setPendingDelete(null); return }
        if (bulkEditOpen) { setBulkEditOpen(false); return }
        if (propertiesHash) { setPropertiesHash(null); return }
        if (addOpen) { setAddOpen(false); return }
        if (detailHash) { setDetailHash(null); return }
        if (selected.size > 0) { setSelected(new Set()); return }
      }
      if (e.key === '?' && !(e.target instanceof HTMLInputElement) && !(e.target instanceof HTMLTextAreaElement)) {
        setHelpOpen(true)
        return
      }
      // 'a' key to open add dialog when not in an input
      if (e.key === 'a' && !(e.target instanceof HTMLInputElement) && !(e.target instanceof HTMLTextAreaElement)) {
        setAddOpen(true)
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [addOpen, bulkEditOpen, contextMenu, detailHash, helpOpen, pendingDelete, propertiesHash, selected.size])

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
  const propertiesTorrent: TorrentSummary | undefined =
    propertiesHash ? torrents.find(t => t.hash === propertiesHash) : undefined

  async function runBulk(action: 'start' | 'stop' | 'recheck' | 'reannounce') {
    const hashes = [...selected]
    if (hashes.length === 0) return
    setToolbarBusy(true)
    try {
      await api.bulk(action, hashes, false)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
    } finally {
      setToolbarBusy(false)
    }
  }

  async function toggleSequential(hashes: string[]) {
    if (hashes.length === 0) return
    await api.torrents.toggleSequential(hashes)
    qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
  }

  async function runTorrent(torrent: TorrentSummary, action: 'start' | 'stop' | 'recheck' | 'reannounce') {
    const actions = {
      start: api.torrents.start,
      stop: api.torrents.stop,
      recheck: api.torrents.recheck,
      reannounce: api.torrents.reannounce,
    }
    await actions[action](torrent.hash)
    qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
  }

  async function deleteTorrent(torrent: TorrentSummary, deleteFiles: boolean) {
    await api.torrents.remove(torrent.hash, deleteFiles)
    setPendingDelete(null)
    setSelected(prev => {
      const next = new Set(prev)
      next.delete(torrent.hash)
      return next
    })
    if (detailHash === torrent.hash) setDetailHash(null)
    qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
  }

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
    setLiveStats({ upload_speed: 0, download_speed: 0 })
    qc.clear()
  }

  function updateMediaInference(mode: MediaInferenceMode) {
    try {
      localStorage.setItem(MEDIA_INFERENCE_KEY, mode)
    } catch {
      // Ignore storage failures; the setting still applies for this tab.
    }
    setMediaInference(mode)
  }

  if (!activeTab.isActive) {
    return <StandbyScreen onTakeOver={activeTab.takeOver} />
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
        <button onClick={() => setHelpOpen(true)} title="Help and links" style={{
          background: 'transparent', border: '1px solid #334155', borderRadius: 5,
          color: '#94a3b8', padding: '3px 10px', fontSize: 12, cursor: 'pointer',
        }}>Help</button>

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
          ↓ {fmtSpeed(liveStats.download_speed)}
        </span>
        <span style={{ fontSize: 11, color: '#22c55e' }}>
          ↑ {fmtSpeed(liveStats.upload_speed)}
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
        <TorrentToolbar
          selectedCount={selected.size}
          onAdd={() => setAddOpen(true)}
          onStart={() => runBulk('start')}
          onStop={() => runBulk('stop')}
          onRecheck={() => runBulk('recheck')}
          onReannounce={() => runBulk('reannounce')}
          onProperties={() => setPropertiesHash([...selected][0] ?? null)}
          onEditSelected={() => setBulkEditOpen(true)}
          onSequential={() => toggleSequential([...selected])}
          onHelp={() => setHelpOpen(true)}
          busy={toolbarBusy}
        />
      )}
      {view === 'torrents' && selected.size > 0 && (
        <BulkActionBar hashes={[...selected]} onClear={() => setSelected(new Set())} />
      )}

      {/* Main content */}
      <main style={{ flex: 1, display: 'flex', overflow: 'hidden' }}>
        {view === 'settings' && (
          <SettingsView
            section={settingsSection}
            onSection={setSettingsSection}
            mediaInference={mediaInference}
            onMediaInference={updateMediaInference}
          />
        )}

        {view === 'torrents' && (
          <>
            <TorrentSidebar
              params={params}
              total={total}
              mediaInference={mediaInference}
              onChange={updateParams}
              onApply={applySavedView}
            />
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
                  onContextMenu={(torrent, x, y) => {
                    setContextMenu({ torrent, x, y })
                    setSelected(prev => prev.has(torrent.hash) ? prev : new Set([torrent.hash]))
                  }}
                  onSort={handleSort}
                  onLoadMore={() => query.fetchNextPage()}
                  hasMore={query.hasNextPage ?? false}
                  isFetchingMore={query.isFetchingNextPage}
                  detailHash={detailHash}
                  mediaInference={mediaInference}
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

      <StatusBar
        loaded={torrents.length}
        total={total}
        selected={selected.size}
        stats={liveStats}
        rtorrent={health?.rtorrent ?? 'connecting'}
        cached={health?.cached_torrents}
        storage={storage?.roots?.[0]}
      />

      {addOpen && <AddTorrentDialog onClose={() => setAddOpen(false)} />}
      {helpOpen && <HelpDialog onClose={() => setHelpOpen(false)} />}
      {contextMenu && (
        <TorrentContextMenu
          menu={contextMenu}
          onClose={() => setContextMenu(null)}
          onProperties={() => setPropertiesHash(contextMenu.torrent.hash)}
          onEditSelected={() => setBulkEditOpen(true)}
          onDetail={() => setDetailHash(contextMenu.torrent.hash)}
          onStart={() => runTorrent(contextMenu.torrent, 'start')}
          onStop={() => runTorrent(contextMenu.torrent, 'stop')}
          onRecheck={() => runTorrent(contextMenu.torrent, 'recheck')}
          onReannounce={() => runTorrent(contextMenu.torrent, 'reannounce')}
          onDelete={() => setPendingDelete(contextMenu.torrent)}
          onCopyHash={() => navigator.clipboard?.writeText(contextMenu.torrent.hash)}
          onCopyName={() => navigator.clipboard?.writeText(contextMenu.torrent.name)}
          onToggleSequential={() => toggleSequential([contextMenu.torrent.hash])}
        />
      )}
      {propertiesTorrent && (
        <TorrentPropertiesDialog torrent={propertiesTorrent} onClose={() => setPropertiesHash(null)} />
      )}
      {bulkEditOpen && selected.size > 0 && (
        <BulkEditDialog hashes={[...selected]} onClose={() => setBulkEditOpen(false)} />
      )}
      {pendingDelete && (
        <DeleteDialog
          torrent={pendingDelete}
          onCancel={() => setPendingDelete(null)}
          onRemove={() => deleteTorrent(pendingDelete, false)}
          onRemoveFiles={() => deleteTorrent(pendingDelete, true)}
        />
      )}
    </div>
  )
}

function StandbyScreen({ onTakeOver }: { onTakeOver: () => void }) {
  return (
    <div style={{
      minHeight: '100vh', background: '#0d1117', color: '#e2e8f0',
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <div style={{
        width: 'min(460px, 100%)', border: '1px solid #1e2433', borderRadius: 8,
        background: '#0f141d', padding: 20, display: 'flex', flexDirection: 'column', gap: 12,
      }}>
        <div style={{ fontWeight: 700, fontSize: 18 }}>rtorrentNG is open in another tab</div>
        <div style={{ color: '#94a3b8', fontSize: 13, lineHeight: 1.45 }}>
          This standby tab is not connected to the API or websocket. Use one active tab for large libraries.
        </div>
        <button onClick={onTakeOver} style={{
          width: 'fit-content', background: '#1e3a5f', border: '1px solid #3b82f6',
          borderRadius: 5, color: '#bfdbfe', padding: '7px 11px', fontSize: 13, cursor: 'pointer',
        }}>Take over this tab</button>
      </div>
    </div>
  )
}

function SettingsView({ section, onSection, mediaInference, onMediaInference }: {
  section: SettingsSection
  onSection: (section: SettingsSection) => void
  mediaInference: MediaInferenceMode
  onMediaInference: (mode: MediaInferenceMode) => void
}) {
  const sections: Array<[SettingsSection, string]> = [
    ['library', 'Library'],
    ['engine', 'Engine'],
    ['automation', 'Automation'],
    ['support', 'Support'],
  ]
  return (
    <>
      <aside style={{
        width: 220, flexShrink: 0, background: '#0f141d', borderRight: '1px solid #1e2433',
        padding: 12,
      }}>
        <div style={{ fontSize: 16, fontWeight: 700, color: '#e2e8f0', margin: '4px 4px 12px' }}>Settings</div>
        {sections.map(([key, label]) => (
          <button key={key} onClick={() => onSection(key)} style={{
            width: '100%', display: 'block', textAlign: 'left', marginBottom: 4,
            background: section === key ? '#1e3a5f' : 'transparent',
            border: '1px solid ' + (section === key ? '#3b82f6' : 'transparent'),
            borderRadius: 5, color: section === key ? '#bfdbfe' : '#94a3b8',
            padding: '7px 9px', fontSize: 13, cursor: 'pointer',
          }}>{label}</button>
        ))}
      </aside>
      <div style={{ flex: 1, overflowY: 'auto', background: '#0f1117' }}>
        {section === 'library' && (<>
          <PanelTitle title="Library" subtitle="Categories, storage roots, and tracker summaries" />
          <PanelFrame><CategoriesPanel /></PanelFrame>
          <PanelFrame><StoragePanel /></PanelFrame>
          <PanelFrame><TrackerHealthPanel /></PanelFrame>
        </>)}
        {section === 'engine' && (<>
          <PanelTitle title="Engine" subtitle="Runtime diagnostics, user agent, and capability checks" />
          <PanelFrame><EnginePanel /></PanelFrame>
          <PanelFrame><UserAgentPanel /></PanelFrame>
        </>)}
        {section === 'automation' && (<>
          <PanelTitle title="Automation" subtitle="Ratio groups, workflows, and RSS rules" />
          <PanelFrame><RatioGroupsPanel /></PanelFrame>
          <PanelFrame><WorkflowsPanel /></PanelFrame>
          <PanelFrame><RssRulesPanel /></PanelFrame>
        </>)}
        {section === 'support' && (<>
          <PanelTitle title="Support" subtitle="Project resources and community support" />
          <PanelFrame>
            <AppearancePanel mediaInference={mediaInference} onMediaInference={onMediaInference} />
          </PanelFrame>
          <div style={{ padding: 18, display: 'grid', gap: 10, maxWidth: 720 }}>
            <a style={supportLink} href="https://discord.gg/4ub88HeHFm" target="_blank" rel="noreferrer">Discord support</a>
            <a style={supportLink} href="https://github.com/rtorrentng/rtorrentng" target="_blank" rel="noreferrer">GitHub project</a>
            <button onClick={() => window.dispatchEvent(new KeyboardEvent('keydown', { key: '?' }))} style={supportButton}>Open help</button>
          </div>
        </>)}
      </div>
    </>
  )
}

function PanelTitle({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div style={{ padding: '18px 22px', borderBottom: '1px solid #1e2433' }}>
      <div style={{ fontSize: 17, fontWeight: 700, color: '#e2e8f0' }}>{title}</div>
      <div style={{ marginTop: 3, fontSize: 12, color: '#64748b' }}>{subtitle}</div>
    </div>
  )
}

function PanelFrame({ children }: { children: React.ReactNode }) {
  return <div style={{ borderBottom: '1px solid #1e2433' }}>{children}</div>
}

function DeleteDialog({ torrent, onCancel, onRemove, onRemoveFiles }: {
  torrent: TorrentSummary
  onCancel: () => void
  onRemove: () => void
  onRemoveFiles: () => void
}) {
  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1150,
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <div style={{
        width: 'min(480px, 100%)', background: '#0f141d', border: '1px solid #7f1d1d',
        borderRadius: 8, boxShadow: '0 24px 60px rgba(0,0,0,0.5)',
      }}>
        <div style={{ padding: 16, borderBottom: '1px solid #1e2433' }}>
          <div style={{ fontSize: 16, fontWeight: 700, color: '#fecaca' }}>Delete torrent</div>
          <div style={{ marginTop: 8, color: '#cbd5e1', fontSize: 13, lineHeight: 1.4, wordBreak: 'break-word' }}>{torrent.name}</div>
        </div>
        <div style={{ padding: 14, display: 'flex', justifyContent: 'flex-end', gap: 8 }}>
          <button onClick={onCancel} style={dialogButton('#64748b')}>Cancel</button>
          <button onClick={onRemove} style={dialogButton('#f87171')}>Remove torrent</button>
          <button onClick={onRemoveFiles} style={dialogButton('#ef4444')}>Delete files</button>
        </div>
      </div>
    </div>
  )
}

const supportLink: React.CSSProperties = {
  color: '#93c5fd',
  textDecoration: 'none',
  fontSize: 14,
}

const supportButton: React.CSSProperties = {
  width: 'fit-content',
  background: '#1e3a5f',
  border: '1px solid #3b82f6',
  borderRadius: 5,
  color: '#bfdbfe',
  padding: '6px 10px',
  fontSize: 13,
  cursor: 'pointer',
}

function dialogButton(color: string): React.CSSProperties {
  return {
    background: '#1e2433',
    border: `1px solid ${color}66`,
    borderRadius: 5,
    color,
    padding: '6px 10px',
    fontSize: 12,
    cursor: 'pointer',
  }
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
