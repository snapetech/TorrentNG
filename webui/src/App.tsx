import { Suspense, lazy, useState, useCallback, useEffect } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { useTorrentsInfinite, flattenPages, useHealth } from './hooks/useTorrents'
import { useWebSocket } from './hooks/useWebSocket'
import { TorrentTable } from './components/TorrentTable'
import { FilterBar } from './components/FilterBar'
import { SavedViewsBar } from './components/SavedViewsBar'
import { TorrentDetail } from './components/TorrentDetail'
import { AddTorrentDialog } from './components/AddTorrentDialog'
import { CategoriesPanel } from './components/CategoriesPanel'
import { TorrentSidebar } from './components/TorrentSidebar'
import { TrackerHealthPanel } from './components/TrackerHealthPanel'
import { TorrentToolbar } from './components/TorrentToolbar'
import { TorrentContextMenu, type ContextMenuState } from './components/TorrentContextMenu'
import { HelpDialog } from './components/HelpDialog'
import { StatusBar } from './components/StatusBar'
import { TorrentPropertiesDialog } from './components/TorrentPropertiesDialog'
import { AppearancePanel, type MediaInferenceMode } from './components/AppearancePanel'
import { BulkEditDialog } from './components/BulkEditDialog'
import { api, AuthError, type ListParams, type LiveStats, type TorrentSummary } from './api/client'
import { PALETTES, applyTheme, findPalette, THEME_MODE_STORAGE_KEY, THEME_STORAGE_KEY, type ThemeMode } from './themes'

const EnginePanel = lazy(() => import('./components/EnginePanel').then(module => ({ default: module.EnginePanel })))
const StoragePanel = lazy(() => import('./components/StoragePanel').then(module => ({ default: module.StoragePanel })))
const UserAgentPanel = lazy(() => import('./components/UserAgentPanel').then(module => ({ default: module.UserAgentPanel })))
const RatioGroupsPanel = lazy(() => import('./components/RatioGroupsPanel').then(module => ({ default: module.RatioGroupsPanel })))
const WorkflowsPanel = lazy(() => import('./components/WorkflowsPanel').then(module => ({ default: module.WorkflowsPanel })))
const RssRulesPanel = lazy(() => import('./components/RssRulesPanel').then(module => ({ default: module.RssRulesPanel })))
const LogsPanel = lazy(() => import('./components/LogsPanel').then(module => ({ default: module.LogsPanel })))

type View = 'torrents' | 'settings'
type AuthState = 'checking' | 'authenticated' | 'unauthenticated'
type SettingsSection = 'library' | 'engine' | 'automation' | 'support'
const MEDIA_INFERENCE_KEY = 'tng.mediaInference'
const DETAIL_AUTO_DISPLAY_KEY = 'tng.detailAutoDisplay'

function utpPathLabel(path: string): string {
  switch (path) {
    case 'outgoing_peer_wire': return 'outgoing'
    case 'metadata_fetch': return 'metadata'
    case 'incoming_peer_wire': return 'incoming'
    default: return path.replaceAll('_', ' ')
  }
}

function utpStatus(capabilities: NonNullable<Awaited<ReturnType<typeof api.health>>['engine']>['capabilities'] | undefined) {
  const networking = capabilities?.networking
  if (!networking) return null
  const paths = networking.utp_transport_paths ?? []
  const label = paths.length
    ? paths.map(utpPathLabel).join('+')
    : networking.utp_transport ? 'enabled' : 'off'
  const policy = [
    networking.utp_outgoing_policy ? `outgoing=${networking.utp_outgoing_policy}` : null,
    networking.utp_metadata_policy ? `metadata=${networking.utp_metadata_policy}` : null,
    networking.utp_incoming_enabled !== undefined ? `incoming=${networking.utp_incoming_enabled ? 'on' : 'off'}` : null,
  ].filter(Boolean).join(', ')
  const title = paths.length
    ? `Active uTP runtime paths: ${paths.map(utpPathLabel).join(', ')}${policy ? ` (${policy})` : ''}`
    : `No active uTP runtime transport path${policy ? ` (${policy})` : ''}`
  return { label, title, enabled: networking.utp_transport === true }
}
const SETTINGS_SECTION_KEY = 'tng.settingsSection'
const ACTIVE_TAB_KEY = 'tng.activeTab'
const ACTIVE_TAB_TTL_MS = 8000

const preloadSettingsPanels = {
  library: () => void import('./components/StoragePanel'),
  engine: () => {
    void import('./components/EnginePanel')
    void import('./components/UserAgentPanel')
  },
  automation: () => {
    void import('./components/RatioGroupsPanel')
    void import('./components/WorkflowsPanel')
    void import('./components/RssRulesPanel')
  },
  support: () => void import('./components/LogsPanel'),
} satisfies Record<SettingsSection, () => void>

function preloadSettingsSection(section: SettingsSection) {
  preloadSettingsPanels[section]()
}

function preloadAllSettingsPanels() {
  ;(Object.keys(preloadSettingsPanels) as SettingsSection[]).forEach(preloadSettingsSection)
}

function loadThemeId(): string {
  try {
    const value = localStorage.getItem(THEME_STORAGE_KEY)
    return value && findPalette(value).id === value ? value : PALETTES[0].id
  } catch {
    return PALETTES[0].id
  }
}

function loadThemeMode(): ThemeMode {
  try {
    return localStorage.getItem(THEME_MODE_STORAGE_KEY) === 'light' ? 'light' : 'dark'
  } catch {
    return 'dark'
  }
}

function loadMediaInference(): MediaInferenceMode {
  let value: string | null = null
  try {
    value = localStorage.getItem(MEDIA_INFERENCE_KEY)
  } catch {
    value = null
  }
  return value === 'full' || value === 'suffix' || value === 'hints' || value === 'off' ? value : 'full'
}

function loadDetailAutoDisplay(): boolean {
  try {
    return localStorage.getItem(DETAIL_AUTO_DISPLAY_KEY) !== 'off'
  } catch {
    return true
  }
}

function isSettingsSection(value: string | null): value is SettingsSection {
  return value === 'library' || value === 'engine' || value === 'automation' || value === 'support'
}

function loadSettingsSection(): SettingsSection {
  try {
    const hashSection = new URLSearchParams(window.location.hash.replace(/^#/, '')).get('settings')
    if (isSettingsSection(hashSection)) return hashSection
    const stored = localStorage.getItem(SETTINGS_SECTION_KEY)
    return isSettingsSection(stored) ? stored : 'library'
  } catch {
    return 'library'
  }
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
  const [togglingFeature, setTogglingFeature] = useState<'dht' | 'pex' | null>(null)
  const [featureError, setFeatureError] = useState<string | null>(null)
  const [actionNotice, setActionNotice] = useState<{ text: string; tone: 'ok' | 'error' } | null>(null)
  const [settingsSection, setSettingsSection] = useState<SettingsSection>(() => loadSettingsSection())
  const [mediaInference, setMediaInference] = useState<MediaInferenceMode>(loadMediaInference)
  const [themeId, setThemeId] = useState(loadThemeId)
  const [themeMode, setThemeMode] = useState<ThemeMode>(loadThemeMode)
  const [detailAutoDisplay, setDetailAutoDisplay] = useState(loadDetailAutoDisplay)
  const activeTheme = findPalette(themeId)[themeMode]

  const isAuthed = activeTab.isActive && authState === 'authenticated'
  const query = useTorrentsInfinite(params, isAuthed)
  const { torrents, total } = flattenPages(query.data)
  const { data: health } = useHealth(activeTab.isActive && authState === 'authenticated')
  const healthUtp = utpStatus(health?.engine?.capabilities)
  const { data: storage } = useQuery({
    queryKey: ['storage', 'status-bar'],
    queryFn: api.storage,
    enabled: isAuthed,
    refetchInterval: 10_000,
  })
  const { data: transferInfo } = useQuery({
    queryKey: ['transfer-info', 'status-bar'],
    queryFn: api.transferInfo,
    enabled: isAuthed,
    refetchInterval: 2_000,
  })

  const handleStats = useCallback((stats: LiveStats) => setLiveStats(stats), [])
  useWebSocket(handleStats, isAuthed)

  useEffect(() => {
    applyTheme(themeId, themeMode)
    try {
      localStorage.setItem(THEME_STORAGE_KEY, themeId)
      localStorage.setItem(THEME_MODE_STORAGE_KEY, themeMode)
    } catch {
      // Theme selection still applies for this tab.
    }
  }, [themeId, themeMode])

  useEffect(() => {
    if (!transferInfo) return
    setLiveStats(prev => ({
      ...prev,
      download_speed: transferInfo.dl_info_speed ?? 0,
      upload_speed: transferInfo.up_info_speed ?? 0,
      download_total: transferInfo.dl_info_data ?? prev.download_total,
      upload_total: transferInfo.up_info_data ?? prev.upload_total,
    }))
  }, [transferInfo])

  useEffect(() => {
    if (!featureError) return
    const timer = window.setTimeout(() => setFeatureError(null), 5000)
    return () => window.clearTimeout(timer)
  }, [featureError])

  useEffect(() => {
    if (!actionNotice) return
    const timer = window.setTimeout(() => setActionNotice(null), actionNotice.tone === 'error' ? 6000 : 3000)
    return () => window.clearTimeout(timer)
  }, [actionNotice])

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
    try {
      localStorage.setItem(SETTINGS_SECTION_KEY, settingsSection)
      if (view === 'settings') {
        const nextHash = `settings=${settingsSection}`
        if (window.location.hash.replace(/^#/, '') !== nextHash) {
          history.replaceState(null, '', `#${nextHash}`)
        }
      }
    } catch {
      // Section still applies in this tab.
    }
  }, [settingsSection, view])

  useEffect(() => {
    function onHashChange() {
      const section = new URLSearchParams(window.location.hash.replace(/^#/, '')).get('settings')
      if (isSettingsSection(section)) {
        setSettingsSection(section)
        setView('settings')
      }
    }
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

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

  useEffect(() => {
    if (authState !== 'authenticated' || !activeTab.isActive) return
    const win = window as typeof window & { requestIdleCallback?: (callback: () => void, options?: { timeout: number }) => number; cancelIdleCallback?: (id: number) => void }
    if (win.requestIdleCallback) {
      const id = win.requestIdleCallback(preloadAllSettingsPanels, { timeout: 5000 })
      return () => win.cancelIdleCallback?.(id)
    }
    const id = window.setTimeout(preloadAllSettingsPanels, 2500)
    return () => window.clearTimeout(id)
  }, [activeTab.isActive, authState])

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
      if (next.has(hash)) {
        next.delete(hash)
        if (detailHash === hash) setDetailHash(null)
      } else {
        next.add(hash)
        if (detailAutoDisplay) setDetailHash(hash)
      }
      return next
    })
  }

  function handleSelectAll(hashes: string[]) {
    setSelected(new Set(hashes))
    if (detailAutoDisplay && hashes.length === 1) setDetailHash(hashes[0])
    if (hashes.length === 0) setDetailHash(null)
  }

  function updateDetailAutoDisplay(enabled: boolean) {
    try {
      localStorage.setItem(DETAIL_AUTO_DISPLAY_KEY, enabled ? 'on' : 'off')
    } catch {
      // The current tab still honors the setting.
    }
    setDetailAutoDisplay(enabled)
  }

  function openAutoDetail(hash: string | null) {
    if (detailAutoDisplay) setDetailHash(hash)
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
    setActionNotice(null)
    try {
      await api.bulk(action, hashes, false)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      setActionNotice({ text: `${action} queued for ${hashes.length.toLocaleString()} torrent${hashes.length === 1 ? '' : 's'}`, tone: 'ok' })
    } catch {
      setActionNotice({ text: `Failed to ${action} selected torrents`, tone: 'error' })
    } finally {
      setToolbarBusy(false)
    }
  }

  async function toggleSequential(hashes: string[]) {
    if (hashes.length === 0) return
    setActionNotice(null)
    try {
      await api.torrents.toggleSequential(hashes)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      setActionNotice({ text: `Sequential toggled for ${hashes.length.toLocaleString()} torrent${hashes.length === 1 ? '' : 's'}`, tone: 'ok' })
    } catch {
      setActionNotice({ text: 'Failed to toggle sequential download', tone: 'error' })
    }
  }

  async function toggleSessionFeature(feature: 'dht' | 'pex') {
    const current = liveStats[feature]
    if ((current !== 'on' && current !== 'off') || togglingFeature) return
    const enabled = current !== 'on'
    setTogglingFeature(feature)
    setFeatureError(null)
    try {
      const result = await api.session.setFeatures({ [feature]: enabled })
      const refreshed = await api.session.getFeatures().catch(() => result)
      const applied = refreshed[feature] ?? result[feature] ?? enabled
      setLiveStats(prev => ({ ...prev, [feature]: applied ? 'on' : 'off' }))
      qc.invalidateQueries({ queryKey: ['engine'] })
    } catch {
      setFeatureError(`Failed to toggle ${feature.toUpperCase()}`)
    } finally {
      setTogglingFeature(null)
    }
  }

  async function runTorrent(torrent: TorrentSummary, action: 'start' | 'stop' | 'recheck' | 'reannounce') {
    const actions = {
      start: api.torrents.start,
      stop: api.torrents.stop,
      recheck: api.torrents.recheck,
      reannounce: api.torrents.reannounce,
    }
    setActionNotice(null)
    try {
      await actions[action](torrent.hash)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      setActionNotice({ text: `${action} queued`, tone: 'ok' })
    } catch {
      setActionNotice({ text: `Failed to ${action} torrent`, tone: 'error' })
    }
  }

  async function deleteTorrent(torrent: TorrentSummary, deleteFiles: boolean) {
    setActionNotice(null)
    try {
      await api.torrents.remove(torrent.hash, deleteFiles)
      setPendingDelete(null)
      setSelected(prev => {
        const next = new Set(prev)
        next.delete(torrent.hash)
        return next
      })
      if (detailHash === torrent.hash) setDetailHash(null)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      setActionNotice({ text: deleteFiles ? 'Torrent and files deleted' : 'Torrent removed', tone: 'ok' })
    } catch {
      setActionNotice({ text: 'Failed to delete torrent', tone: 'error' })
    }
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
      <div className="tng-card tng-standby-card" style={{
        minHeight: '100vh', background: 'var(--bg)', color: 'var(--faint)',
        display: 'grid', placeItems: 'center', fontSize: 13, padding: 24,
      }}>
        <div style={{
          display: 'grid', gap: 10, justifyItems: 'center',
          border: '1px solid var(--border)', borderRadius: 8,
          background: 'var(--panel)', padding: '18px 22px',
        }}>
          <span style={{
            width: 22, height: 22, borderRadius: '50%',
            border: '2px solid var(--border-strong)', borderTopColor: 'var(--accent)',
            animation: 'tng-spin 800ms linear infinite',
          }} />
          <span>Checking session...</span>
        </div>
      </div>
    )
  }

  if (authState === 'unauthenticated') {
    return <LoginScreen message={authMessage} onLogin={handleLogin} />
  }

  return (
    <div style={{
      display: 'flex', flexDirection: 'column', height: '100vh', width: '100vw',
      overflow: 'hidden', background: 'var(--bg)', color: 'var(--text)',
    }}>
      {/* Topbar */}
      <header className="tng-topbar" style={{
        height: 44, background: 'var(--bg)', borderBottom: '1px solid var(--border)',
        display: 'flex', alignItems: 'center', padding: '0 16px', gap: 12, flexShrink: 0,
        minWidth: 0, overflowX: 'auto', overflowY: 'hidden', scrollbarWidth: 'thin',
      }}>
        <span style={{
          fontWeight: 800, fontSize: 15, color: 'var(--text)', flex: '0 0 auto',
          display: 'inline-flex', alignItems: 'center', gap: 7,
        }}>
          <span style={{
            width: 9, height: 9, borderRadius: 3,
            background: 'linear-gradient(135deg, var(--accent), var(--success))',
            boxShadow: '0 0 14px color-mix(in srgb, var(--accent) 48%, transparent)',
          }} />
          TorrentNG
        </span>

        <nav aria-label="Primary" style={{ display: 'flex', gap: 4, marginLeft: 4, flex: '0 0 auto' }}>
          {(['torrents', 'settings'] as View[]).map(v => (
            <button
              key={v}
              onPointerEnter={() => { if (v === 'settings') preloadSettingsSection(settingsSection) }}
              onFocus={() => { if (v === 'settings') preloadSettingsSection(settingsSection) }}
              onClick={() => {
                if (v === 'settings') preloadSettingsSection(settingsSection)
                setView(v)
              }}
              aria-current={view === v ? 'page' : undefined}
              style={{
              background: view === v ? 'var(--accent-soft)' : 'transparent',
              border: '1px solid ' + (view === v ? 'var(--accent)' : 'transparent'),
              borderRadius: 5, color: view === v ? 'var(--accent-text)' : 'var(--faint)',
              padding: '2px 10px', fontSize: 12, cursor: 'pointer', textTransform: 'capitalize',
              whiteSpace: 'nowrap', flex: '0 0 auto', display: 'inline-flex', alignItems: 'center', gap: 6,
              fontWeight: view === v ? 800 : 600,
            }}>
              <span style={{ color: view === v ? 'var(--accent-text)' : 'var(--accent)' }}>{v === 'torrents' ? '▤' : '⚙'}</span>
              {v}
            </button>
          ))}
        </nav>

        {view === 'torrents' && (
          <span className="tng-topbar-pill" data-tone="neutral" style={{
            color: 'var(--muted)', background: 'var(--surface)', border: '1px solid var(--border)',
            borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700,
            whiteSpace: 'nowrap', flex: '0 0 auto',
          }}>
            {total.toLocaleString()} torrents
          </span>
        )}
        {selected.size > 0 && view === 'torrents' && (
          <span className="tng-topbar-pill" data-tone="accent" style={{
            color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
            borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 800,
            whiteSpace: 'nowrap', flex: '0 0 auto',
          }}>
            {selected.size.toLocaleString()} selected
          </span>
        )}
        <button onClick={() => setHelpOpen(true)} title="Help and links" style={{
          background: 'transparent', border: '1px solid var(--border-strong)', borderRadius: 5,
          color: 'var(--muted)', padding: '3px 10px', fontSize: 12, cursor: 'pointer',
          whiteSpace: 'nowrap', flex: '0 0 auto',
        }}>Help</button>

        <span className="tng-topbar-pill" data-tone={health?.backend?.status === 'connected' ? 'ok' : 'error'} title="Selected backend connection state" style={{
          fontSize: 11, color: health?.backend?.status === 'connected' ? 'var(--success)' : 'var(--danger)',
          display: 'flex', alignItems: 'center', gap: 5, padding: '2px 7px',
          border: '1px solid ' + (health?.backend?.status === 'connected' ? 'color-mix(in srgb, var(--success) 42%, var(--border))' : 'color-mix(in srgb, var(--danger) 42%, var(--border))'),
          borderRadius: 999,
          background: health?.backend?.status === 'connected' ? 'color-mix(in srgb, var(--success) 9%, transparent)' : 'color-mix(in srgb, var(--danger) 9%, transparent)',
        }}>
          <span style={{
            width: 6, height: 6, borderRadius: '50%',
            background: health?.backend?.status === 'connected' ? 'var(--success)' : 'var(--danger)',
            display: 'inline-block',
          }} />
          {health?.backend ? `${health.backend.type}: ${health.backend.status}` : 'connecting...'}
        </span>

        {healthUtp && (
          <span className="tng-topbar-pill" data-tone={healthUtp.enabled ? 'ok' : 'neutral'} title={healthUtp.title} style={{
            fontSize: 11,
            color: healthUtp.enabled ? 'var(--success)' : 'var(--muted)',
            display: 'flex', alignItems: 'center', gap: 5, padding: '2px 7px',
            border: '1px solid ' + (healthUtp.enabled ? 'color-mix(in srgb, var(--success) 42%, var(--border))' : 'var(--border)'),
            borderRadius: 999,
            background: healthUtp.enabled ? 'color-mix(in srgb, var(--success) 9%, transparent)' : 'var(--surface)',
          }}>
            uTP {healthUtp.label}
          </span>
        )}

        <span className="tng-topbar-spacer" style={{ flex: '1 0 12px' }} />
        <div className="tng-theme-controls" style={{ display: 'flex', alignItems: 'center', gap: 6, flex: '0 0 auto' }}>
          <span
            className="tng-theme-swatch"
            title={`${findPalette(themeId).label} ${themeMode}`}
            aria-hidden="true"
            style={{
              ['--swatch-bg' as string]: activeTheme.bg,
              ['--swatch-panel' as string]: activeTheme.panel,
              ['--swatch-surface' as string]: activeTheme.surface,
              ['--swatch-accent' as string]: activeTheme.accent,
            }}
          >
            <span />
            <span />
            <span />
          </span>
          <select
            aria-label="Theme palette"
            value={themeId}
            onChange={e => setThemeId(e.target.value)}
            style={themeSelectStyle}
          >
            {PALETTES.map(palette => (
              <option key={palette.id} value={palette.id}>{palette.label}</option>
            ))}
          </select>
          <button
            onClick={() => setThemeMode(mode => mode === 'dark' ? 'light' : 'dark')}
            title="Toggle light/dark theme"
            style={themeButtonStyle}
          >
            {themeMode === 'dark' ? 'Dark' : 'Light'}
          </button>
        </div>
        <button onClick={handleLogout} title="Log out" style={{
          background: 'transparent', border: '1px solid var(--border-strong)', borderRadius: 5,
          color: 'var(--muted)', padding: '3px 10px', fontSize: 12, cursor: 'pointer',
          whiteSpace: 'nowrap', flex: '0 0 auto',
        }}>Log out</button>
      </header>

      {view === 'torrents' && (
        <FilterBar params={params} onChange={updateParams} />
      )}
      {view === 'torrents' && (
        <SavedViewsBar params={params} onApply={applySavedView} />
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
          onClearSelection={() => setSelected(new Set())}
          onHelp={() => setHelpOpen(true)}
          busy={toolbarBusy}
        />
      )}
      {/* Main content */}
      <main className="tng-main" style={{ flex: 1, minWidth: 0, display: 'flex', overflow: 'hidden' }}>
        {view === 'settings' && (
          <SettingsView
            section={settingsSection}
            onSection={setSettingsSection}
            mediaInference={mediaInference}
            onMediaInference={updateMediaInference}
            themeId={themeId}
            themeMode={themeMode}
            onTheme={setThemeId}
            onThemeMode={setThemeMode}
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
            <div style={{ flex: 1, minWidth: 0, display: 'flex', flexDirection: 'column', overflow: 'hidden' }}>
              {query.isError && (
                <div style={{
                  padding: 24, color: 'var(--danger)', textAlign: 'center',
                  display: 'grid', placeItems: 'center', gap: 10,
                }}>
                  <div style={{
                    border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
                    background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
                    borderRadius: 8, padding: '16px 18px', display: 'grid', gap: 8, minWidth: 280,
                  }}>
                    <span style={{ fontWeight: 800 }}>Failed to connect to TorrentNG API.</span>
                    <span style={{ color: 'var(--faint)', fontSize: 12 }}>The table will refresh when the API responds again.</span>
                  </div>
                  <button onClick={() => query.refetch()} style={{
                    background: 'var(--surface-2)', border: '1px solid var(--border-strong)',
                    borderRadius: 5, color: 'var(--muted)', padding: '5px 10px', fontSize: 12,
                    cursor: 'pointer',
                  }}>Retry</button>
                </div>
              )}
              {query.isLoading && !query.data && (
                <div style={{
                  padding: 24, color: 'var(--faint)', display: 'grid', gap: 10,
                  alignContent: 'start',
                }}>
                  {Array.from({ length: 8 }, (_, index) => (
                    <div key={index} style={{
                      border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)',
                      padding: '10px 12px', display: 'grid', gap: 8,
                    }}>
                      <span className="tng-skeleton" style={{ width: index % 2 ? '44%' : '62%', height: 12 }} />
                      <span className="tng-skeleton" style={{ width: index % 3 ? '72%' : '38%', height: 8 }} />
                    </div>
                  ))}
                </div>
              )}
              {query.data && (
                <TorrentTable
                  torrents={torrents}
                  total={total}
                  selected={selected}
                  params={params}
                  onSelect={handleSelect}
                  onSelectAll={handleSelectAll}
                  onDetail={openAutoDetail}
                  onContextMenu={(torrent, x, y) => {
                    setContextMenu({ torrent, x, y })
                    setSelected(prev => {
                      if (prev.has(torrent.hash)) return prev
                      const next = new Set(prev)
                      next.add(torrent.hash)
                      return next
                    })
                    if (detailAutoDisplay) setDetailHash(torrent.hash)
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
                autoDisplay={detailAutoDisplay}
                onAutoDisplayChange={updateDetailAutoDisplay}
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
        backendType={health?.backend?.type ?? 'backend'}
        backendStatus={health?.backend?.status ?? health?.rtorrent ?? 'connecting'}
        cached={health?.cached_torrents}
        storage={storage?.roots?.[0]}
        utpLabel={healthUtp?.label}
        utpTitle={healthUtp?.title}
        utpEnabled={healthUtp?.enabled}
        togglingFeature={togglingFeature}
        featureError={featureError}
        actionMessage={actionNotice?.text}
        actionTone={actionNotice?.tone}
        onToggleDht={() => toggleSessionFeature('dht')}
        onTogglePex={() => toggleSessionFeature('pex')}
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
      minHeight: '100vh', background: 'var(--bg)', color: 'var(--text)',
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <div style={{
        width: 'min(460px, 100%)', border: '1px solid var(--border)', borderRadius: 8,
        background: 'var(--panel)', padding: 20, display: 'flex', flexDirection: 'column', gap: 12,
        boxShadow: '0 24px 60px var(--shadow)',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
          <span style={{
            width: 30, height: 30, borderRadius: 8, display: 'inline-grid', placeItems: 'center',
            color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
            fontWeight: 900,
          }}>▣</span>
          <div style={{ fontWeight: 800, fontSize: 18 }}>TorrentNG is open in another tab</div>
        </div>
        <div style={{ color: 'var(--muted)', fontSize: 13, lineHeight: 1.45 }}>
          This standby tab is not connected to the API or websocket. Use one active tab for large libraries.
        </div>
        <button onClick={onTakeOver} style={{
          width: 'fit-content', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
          borderRadius: 5, color: 'var(--accent-text)', padding: '7px 11px', fontSize: 13, cursor: 'pointer',
        }}>Take over this tab</button>
      </div>
    </div>
  )
}

function SettingsView({ section, onSection, mediaInference, onMediaInference, themeId, themeMode, onTheme, onThemeMode }: {
  section: SettingsSection
  onSection: (section: SettingsSection) => void
  mediaInference: MediaInferenceMode
  onMediaInference: (mode: MediaInferenceMode) => void
  themeId: string
  themeMode: ThemeMode
  onTheme: (id: string) => void
  onThemeMode: (mode: ThemeMode) => void
}) {
  const sections: Array<[SettingsSection, string, string]> = [
    ['library', 'Library', '▦'],
    ['engine', 'Backend', '⚙'],
    ['automation', 'Automation', '⟲'],
    ['support', 'Support', '?'],
  ]
  function moveSection(current: SettingsSection, delta: number) {
    const index = sections.findIndex(([key]) => key === current)
    const next = sections[(index + delta + sections.length) % sections.length][0]
    preloadSettingsSection(next)
    onSection(next)
    window.setTimeout(() => document.getElementById(`settings-tab-${next}`)?.focus(), 0)
  }
  return (
    <>
      <aside className="tng-settings-sidebar" style={{
        width: 220, flexShrink: 0, background: 'var(--panel)', borderRight: '1px solid var(--border)',
        padding: 12,
      }}>
        <div style={{ margin: '4px 4px 12px' }}>
          <div style={{ fontSize: 16, fontWeight: 800, color: 'var(--text)' }}>Settings</div>
          <div style={{ fontSize: 11, color: 'var(--faint)', marginTop: 3 }}>Daemon, library, and browser controls</div>
        </div>
        <div role="tablist" aria-label="Settings sections" aria-orientation="vertical">
        {sections.map(([key, label, icon]) => (
          <button
            key={key}
            type="button"
            id={`settings-tab-${key}`}
            role="tab"
            className="tng-settings-nav-button"
            data-active={section === key ? 'true' : 'false'}
            onPointerEnter={() => preloadSettingsSection(key)}
            onFocus={() => preloadSettingsSection(key)}
            onClick={() => {
              preloadSettingsSection(key)
              onSection(key)
            }}
            onKeyDown={event => {
              if (event.key === 'Enter' || event.key === ' ') {
                event.preventDefault()
                preloadSettingsSection(key)
                onSection(key)
              }
              if (event.key === 'ArrowDown' || event.key === 'ArrowRight') {
                event.preventDefault()
                moveSection(key, 1)
              }
              if (event.key === 'ArrowUp' || event.key === 'ArrowLeft') {
                event.preventDefault()
                moveSection(key, -1)
              }
              if (event.key === 'Home') {
                event.preventDefault()
                moveSection(sections[0][0], 0)
              }
              if (event.key === 'End') {
                event.preventDefault()
                moveSection(sections[sections.length - 1][0], 0)
              }
            }}
            aria-selected={section === key}
            aria-controls={`settings-panel-${key}`}
            tabIndex={section === key ? 0 : -1}
            style={{
            width: '100%', display: 'grid', gridTemplateColumns: '22px 1fr auto', alignItems: 'center',
            textAlign: 'left', marginBottom: 4, gap: 7,
            background: section === key ? 'var(--accent-soft)' : 'transparent',
            border: '1px solid ' + (section === key ? 'var(--accent)' : 'transparent'),
            borderRadius: 5, color: section === key ? 'var(--accent-text)' : 'var(--muted)',
            padding: '7px 9px', fontSize: 13, cursor: 'pointer',
          }}>
            <span style={{ color: section === key ? 'var(--accent-text)' : 'var(--accent)', textAlign: 'center' }}>{icon}</span>
            <span>{label}</span>
            {section === key && <span style={{ color: 'var(--accent-text)', fontSize: 10 }}>●</span>}
          </button>
        ))}
        </div>
      </aside>
      <div style={{ flex: 1, overflowY: 'auto', background: 'var(--bg)' }}>
        <Suspense fallback={<SettingsPanelFallback />}>
        {section === 'library' && (<section id="settings-panel-library" role="tabpanel" aria-labelledby="settings-tab-library" tabIndex={0}>
          <PanelTitle title="Library" subtitle="Categories, storage roots, and tracker summaries" />
          <PanelFrame><CategoriesPanel /></PanelFrame>
          <PanelFrame><StoragePanel /></PanelFrame>
          <PanelFrame><TrackerHealthPanel /></PanelFrame>
        </section>)}
        {section === 'engine' && (<section id="settings-panel-engine" role="tabpanel" aria-labelledby="settings-tab-engine" tabIndex={0}>
          <PanelTitle title="Backend" subtitle="Runtime diagnostics, settings, and capability checks" />
          <PanelFrame><EnginePanel /></PanelFrame>
          <PanelFrame><UserAgentPanel /></PanelFrame>
        </section>)}
        {section === 'automation' && (<section id="settings-panel-automation" role="tabpanel" aria-labelledby="settings-tab-automation" tabIndex={0}>
          <PanelTitle title="Automation" subtitle="Ratio groups, workflows, and RSS rules" />
          <PanelFrame><RatioGroupsPanel /></PanelFrame>
          <PanelFrame><WorkflowsPanel /></PanelFrame>
          <PanelFrame><RssRulesPanel /></PanelFrame>
        </section>)}
        {section === 'support' && (<section id="settings-panel-support" role="tabpanel" aria-labelledby="settings-tab-support" tabIndex={0}>
          <PanelTitle title="Support" subtitle="Project resources and community support" />
          <PanelFrame><LogsPanel /></PanelFrame>
          <PanelFrame>
            <AppearancePanel
              mediaInference={mediaInference}
              onMediaInference={onMediaInference}
              themes={PALETTES}
              themeId={themeId}
              themeMode={themeMode}
              onTheme={onTheme}
              onThemeMode={onThemeMode}
            />
          </PanelFrame>
          <div style={{
            padding: 18, display: 'grid', gap: 10, maxWidth: 860,
            gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))',
          }}>
            <SupportCard icon="☊" title="Discord support" href="https://discord.gg/4ub88HeHFm" detail="Community support and release discussion" />
            <SupportCard icon="⌘" title="GitHub project" href="https://github.com/snapetech/TorrentNG" detail="Source, issues, and deployment files" />
            <button onClick={() => window.dispatchEvent(new KeyboardEvent('keydown', { key: '?' }))} style={{
              ...supportButton, width: '100%', minHeight: 72, textAlign: 'left',
              display: 'grid', gridTemplateColumns: '32px 1fr', alignItems: 'center',
            }}>
              <span style={{ fontSize: 18, textAlign: 'center' }}>?</span>
              <span>
                <span style={{ display: 'block', fontWeight: 700 }}>Open help</span>
                <span style={{ display: 'block', color: 'var(--faint)', fontSize: 12, marginTop: 2 }}>Shortcuts and workflow notes</span>
              </span>
            </button>
          </div>
        </section>)}
        </Suspense>
      </div>
    </>
  )
}

function SettingsPanelFallback() {
  return (
    <div role="status" aria-live="polite" style={{
      padding: 24,
      display: 'grid',
      gap: 10,
      color: 'var(--faint)',
      fontSize: 12,
    }}>
      <span className="tng-skeleton" style={{ width: 180, height: 12 }} />
      <span className="tng-skeleton" style={{ width: 'min(520px, 82%)', height: 10 }} />
      <span>Loading settings panel...</span>
    </div>
  )
}

function PanelTitle({ title, subtitle }: { title: string; subtitle: string }) {
  return (
    <div style={{
      padding: '18px 22px', borderBottom: '1px solid var(--border)',
      background: 'linear-gradient(180deg, color-mix(in srgb, var(--surface) 72%, transparent), transparent)',
    }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10 }}>
        <div style={{
          width: 8, height: 24, borderRadius: 99,
          background: 'linear-gradient(180deg, var(--accent), var(--success))',
        }} />
        <div>
          <div style={{ fontSize: 17, fontWeight: 800, color: 'var(--text)' }}>{title}</div>
          <div style={{ marginTop: 3, fontSize: 12, color: 'var(--faint)' }}>{subtitle}</div>
        </div>
      </div>
    </div>
  )
}

function PanelFrame({ children }: { children: React.ReactNode }) {
  return <div style={{ borderBottom: '1px solid var(--border)' }}>{children}</div>
}

function DeleteDialog({ torrent, onCancel, onRemove, onRemoveFiles }: {
  torrent: TorrentSummary
  onCancel: () => void
  onRemove: () => void
  onRemoveFiles: () => void
}) {
  return (
    <div className="tng-modal-backdrop" role="presentation" onMouseDown={e => {
      if (e.target === e.currentTarget) onCancel()
    }} style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1150,
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <div className="tng-modal tng-delete-dialog" role="dialog" aria-modal="true" aria-label={`Delete ${torrent.name}`} style={{
        width: 'min(480px, 100%)', background: 'var(--panel)', border: '1px solid var(--danger)',
        borderRadius: 8, boxShadow: '0 24px 60px var(--shadow)',
      }}>
        <div style={{ padding: 16, borderBottom: '1px solid var(--border)' }}>
          <div style={{ display: 'flex', alignItems: 'center', gap: 9 }}>
            <span style={{
              display: 'inline-grid', placeItems: 'center', width: 26, height: 26,
              borderRadius: 999, background: 'color-mix(in srgb, var(--danger) 14%, transparent)',
              border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
              color: 'var(--danger)', fontWeight: 800,
            }}>!</span>
            <div style={{ fontSize: 16, fontWeight: 800, color: 'var(--danger)' }}>Delete torrent</div>
          </div>
          <div style={{ marginTop: 8, color: 'var(--text)', fontSize: 13, lineHeight: 1.4, wordBreak: 'break-word' }}>{torrent.name}</div>
          <div style={{
            marginTop: 10, color: 'var(--faint)', fontSize: 12, lineHeight: 1.45,
            border: '1px solid var(--border)', borderRadius: 6, background: 'var(--surface)', padding: 9,
          }}>
            Removing only the torrent keeps downloaded files. Deleting files removes the saved payload from disk.
          </div>
        </div>
        <div style={{ padding: 14, display: 'flex', justifyContent: 'flex-end', gap: 8, flexWrap: 'wrap' }}>
          <button onClick={onCancel} style={dialogButton('#64748b')}>Cancel</button>
          <button onClick={onRemove} style={dialogButton('#f87171')}>Remove torrent</button>
          <button onClick={onRemoveFiles} style={dialogButton('#ef4444')}>Delete files</button>
        </div>
      </div>
    </div>
  )
}

const supportLink: React.CSSProperties = {
  color: 'var(--accent)',
  textDecoration: 'none',
  fontSize: 14,
}

function SupportCard({ icon, title, href, detail }: { icon: string; title: string; href: string; detail: string }) {
  return (
    <a className="tng-card-link" style={{
      ...supportLink,
      display: 'grid',
      gridTemplateColumns: '32px 1fr',
      alignItems: 'center',
      gap: 10,
      minHeight: 70,
      border: '1px solid var(--border)',
      borderRadius: 7,
      background: 'var(--surface)',
      padding: 12,
    }} href={href} target="_blank" rel="noreferrer">
      <span style={{ color: 'var(--accent)', fontSize: 18, textAlign: 'center' }}>{icon}</span>
      <span>
        <span style={{ display: 'block', color: 'var(--text)', fontWeight: 700 }}>{title}</span>
        <span style={{ display: 'block', color: 'var(--faint)', fontSize: 12, marginTop: 2 }}>{detail}</span>
        <span style={{ display: 'block', color: 'var(--accent)', fontSize: 11, marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{href.replace(/^https?:\/\//, '')}</span>
      </span>
    </a>
  )
}

const supportButton: React.CSSProperties = {
  width: 'fit-content',
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent)',
  borderRadius: 5,
  color: 'var(--accent-text)',
  padding: '6px 10px',
  fontSize: 13,
  cursor: 'pointer',
}

function dialogButton(color: string): React.CSSProperties {
  return {
    background: 'var(--surface-2)',
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
      minHeight: '100vh', background: 'var(--bg)', color: 'var(--text)',
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <form className="tng-card tng-login-card" onSubmit={submit} style={{
        width: 'min(360px, 100%)', border: '1px solid var(--border)', borderRadius: 8,
        background: 'var(--panel)', padding: 20, display: 'flex', flexDirection: 'column', gap: 12,
        boxShadow: '0 24px 60px var(--shadow)',
      }}>
        <div>
          <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
            <span style={{
              width: 10, height: 10, borderRadius: 3,
              background: 'linear-gradient(135deg, var(--accent), var(--success))',
            }} />
            <div style={{ fontWeight: 800, fontSize: 18 }}>TorrentNG</div>
          </div>
          <div style={{ color: 'var(--faint)', fontSize: 12, marginTop: 4 }}>Sign in to manage torrents</div>
        </div>
        <label className="tng-form-card" style={{ display: 'flex', flexDirection: 'column', gap: 5, fontSize: 12, color: 'var(--muted)' }}>
          Username
          <input
            autoFocus
            value={username}
            onChange={e => setUsername(e.target.value)}
            autoComplete="username"
            style={{
              background: 'var(--surface)', border: '1px solid var(--border-strong)', borderRadius: 5,
              color: 'var(--text)', padding: '8px 10px', fontSize: 14,
            }}
          />
        </label>
        <label className="tng-form-card" style={{ display: 'flex', flexDirection: 'column', gap: 5, fontSize: 12, color: 'var(--muted)' }}>
          Password
          <input
            type="password"
            value={password}
            onChange={e => setPassword(e.target.value)}
            autoComplete="current-password"
            style={{
              background: 'var(--surface)', border: '1px solid var(--border-strong)', borderRadius: 5,
              color: 'var(--text)', padding: '8px 10px', fontSize: 14,
            }}
          />
        </label>
        {error && <div style={{
          color: 'var(--danger)', fontSize: 12,
          background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
          border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
          borderRadius: 6, padding: '8px 9px',
        }}>{error}</div>}
        <button disabled={busy} style={{
          marginTop: 4, background: busy ? 'var(--surface-2)' : 'var(--accent-soft)',
          border: '1px solid var(--accent)', borderRadius: 5, color: 'var(--accent-text)',
          padding: '8px 12px', fontSize: 13, cursor: busy ? 'default' : 'pointer',
        }}>
          {busy ? 'Signing in...' : 'Sign in'}
        </button>
      </form>
    </div>
  )
}

const themeSelectStyle: React.CSSProperties = {
  flex: '0 0 auto',
  width: 132,
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '3px 7px',
  fontSize: 12,
  outline: 'none',
}

const themeButtonStyle: React.CSSProperties = {
  flex: '0 0 auto',
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--muted)',
  padding: '3px 10px',
  fontSize: 12,
  cursor: 'pointer',
  whiteSpace: 'nowrap',
}
