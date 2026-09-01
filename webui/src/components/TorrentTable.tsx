import { useRef, useEffect, useMemo, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { TorrentSummary, ListParams } from '../api/client'
import type { MediaInferenceMode } from './AppearancePanel'
import { maskAnnounceUrl } from '../lib/maskUrl'

interface Props {
  torrents: TorrentSummary[]
  total: number
  selected: Set<string>
  params: ListParams
  onSelect: (hash: string) => void
  onSelectAll: (hashes: string[]) => void
  onSelectAllMatching?: () => void
  isSelectingAllMatching?: boolean
  onDetail: (hash: string | null) => void
  onContextMenu: (torrent: TorrentSummary, x: number, y: number) => void
  onSort: (sort: string) => void
  onLoadMore: () => void
  hasMore: boolean
  isFetchingMore: boolean
  detailHash: string | null
  mediaInference: MediaInferenceMode
}

const ROW_HEIGHT = 36
const TABLE_MIN_WIDTH = 1280
const TABLE_CELL_GAP = 8
const TABLE_HORIZONTAL_PADDING = 12

function fmtSize(bytes: number): string {
  if (bytes >= 1e12) return (bytes / 1e12).toFixed(1) + ' TB'
  if (bytes >= 1e9)  return (bytes / 1e9).toFixed(1) + ' GB'
  if (bytes >= 1e6)  return (bytes / 1e6).toFixed(1) + ' MB'
  return (bytes / 1e3).toFixed(0) + ' KB'
}

function fmtSpeed(bps: number): string {
  if (!bps) return '—'
  return fmtSize(bps) + '/s'
}

function fmtDuration(seconds: number): string {
  if (!Number.isFinite(seconds) || seconds < 0) return '—'
  const whole = Math.max(0, Math.floor(seconds))
  const days = Math.floor(whole / 86400)
  const hours = Math.floor((whole % 86400) / 3600)
  const minutes = Math.floor((whole % 3600) / 60)
  const secs = whole % 60
  if (days > 0) return `${days}d ${hours}h`
  if (hours > 0) return `${hours}h ${minutes}m`
  if (minutes > 0) return `${minutes}m ${secs}s`
  return `${secs}s`
}

function remainingBytes(t: TorrentSummary): number {
  return Math.max(0, t.size_bytes - t.bytes_done)
}

function fmtEta(t: TorrentSummary): string {
  if (t.size_bytes <= 0) return '—'
  const remaining = remainingBytes(t)
  if (remaining === 0 || t.complete) return 'Done'
  if (t.down_rate > 0) return fmtDuration(remaining / t.down_rate)
  if (t.is_open && !t.message) return '∞'
  return '—'
}

function priorityLabel(priority: number): string {
  if (priority > 0) return `High (${priority})`
  if (priority < 0) return `Low (${priority})`
  return 'Normal'
}

function shortPath(path: string): string {
  if (!path) return '—'
  if (path.length <= 34) return path
  return `…${path.slice(-33)}`
}

function statusLabel(t: TorrentSummary): { label: string; color: string } {
  if (t.message && !t.is_active) return { label: 'Error', color: 'var(--danger)' }
  if (t.state === 0) return { label: 'Stopped', color: 'var(--faint)' }
  if (t.state === 2) return { label: 'Checking', color: 'var(--warning)' }
  if (t.complete && t.is_active) return { label: 'Seeding', color: 'var(--success)' }
  if (!t.complete && t.is_active) return { label: 'DL', color: 'var(--accent)' }
  if (t.is_open) return { label: 'Stalled', color: 'var(--warning)' }
  return { label: 'Queued', color: 'var(--muted)' }
}

function rowAccent(t: TorrentSummary): string {
  if (t.message && !t.is_active) return 'var(--danger)'
  if (t.state === 0) return 'var(--faint)'
  if (t.state === 2) return 'var(--warning)'
  if (t.complete && t.is_active) return 'var(--success)'
  if (!t.complete && t.is_active) return 'var(--accent)'
  if (t.is_open) return 'var(--warning)'
  return 'transparent'
}

type ColKey =
  | 'check'
  | 'kind'
  | 'name'
  | 'status'
  | 'size'
  | 'progress'
  | 'remaining'
  | 'eta'
  | 'down_rate'
  | 'up_rate'
  | 'seeds'
  | 'peers'
  | 'ratio'
  | 'added'
  | 'completed'
  | 'downloaded'
  | 'uploaded'
  | 'priority'
  | 'category'
  | 'tags'
  | 'tracker'
  | 'path'
  | 'actions'

interface Col { key: ColKey; label: string; width: string; sortKey?: string; required?: boolean }

/** Columns pinned to the left edge while horizontally scrolling, so the
 * torrent's identity stays visible past the swarm/tracker/timestamp columns.
 * Their widths must stay fixed px (not flexible) for the sticky offsets below
 * to be correct. */
const STICKY_KEYS: ColKey[] = ['check', 'kind', 'name']

const COLS: Col[] = [
  { key: 'check',      label: '',          width: '32px', required: true },
  { key: 'kind',       label: 'Type',      width: '52px' },
  { key: 'name',       label: 'Name',      width: '260px', sortKey: 'name' },
  { key: 'status',     label: 'Status',    width: '78px', sortKey: 'status' },
  { key: 'size',       label: 'Size',      width: '78px', sortKey: 'size' },
  { key: 'progress',   label: '%',         width: '72px', sortKey: 'progress' },
  { key: 'remaining',  label: 'Left',      width: '86px', sortKey: 'remaining' },
  { key: 'eta',        label: 'ETA',       width: '76px' },
  { key: 'down_rate',  label: '↓',         width: '92px', sortKey: 'speed_down' },
  { key: 'up_rate',    label: '↑',         width: '92px', sortKey: 'speed_up' },
  { key: 'seeds',      label: 'Seeds',     width: '60px', sortKey: 'seeds' },
  { key: 'peers',      label: 'Peers',     width: '60px', sortKey: 'peers' },
  { key: 'ratio',      label: 'Ratio',     width: '60px', sortKey: 'ratio' },
  { key: 'added',      label: 'Added',     width: '88px', sortKey: 'added' },
  { key: 'completed',  label: 'Completed', width: '96px', sortKey: 'completed' },
  { key: 'downloaded', label: 'Downloaded', width: '92px' },
  { key: 'uploaded',   label: 'Uploaded', width: '86px' },
  { key: 'priority',   label: 'Priority',  width: '82px' },
  { key: 'category',   label: 'Category',  width: '96px' },
  { key: 'tags',       label: 'Tags',      width: '112px' },
  { key: 'tracker',    label: 'Tracker',   width: '140px' },
  { key: 'path',       label: 'Save path', width: 'minmax(160px, 1fr)' },
  { key: 'actions',    label: '',          width: 'minmax(96px, 1fr)', required: true },
]

const MIN_COL_WIDTH = 44
const DEFAULT_ORDER: ColKey[] = COLS.map(c => c.key)

const DEFAULT_VISIBLE: ColKey[] = [
  'check',
  'kind',
  'name',
  'status',
  'size',
  'progress',
  'remaining',
  'eta',
  'down_rate',
  'up_rate',
  'seeds',
  'peers',
  'ratio',
  'added',
  'completed',
  'category',
  'tags',
  'tracker',
]

const COMPACT_VISIBLE: ColKey[] = ['check', 'kind', 'name', 'status', 'progress', 'remaining', 'eta', 'down_rate', 'up_rate', 'seeds', 'peers', 'ratio']

const COLUMN_STORAGE_KEY = 'tng.visibleColumns.v2'

function fmtDate(ts: number): string {
  if (!ts) return '—'
  return new Date(ts * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: '2-digit' })
}

function trackerHost(url: string): string {
  if (!url) return '—'
  try {
    return new URL(url).hostname
  } catch {
    return maskAnnounceUrl(url)
  }
}

interface MediaKind {
  icon: string
  label: string
  color: string
}

function mediaKind(t: TorrentSummary, mode: MediaInferenceMode): MediaKind {
  if (mode === 'off') return { icon: '📦', label: 'Type inference disabled', color: 'var(--faint)' }

  const haystack = mode === 'suffix' ? '' : `${t.name} ${t.category} ${t.tags} ${t.directory}`.toLowerCase()
  const suffixes = mode === 'hints' ? [] : fileSuffixes(`${t.name} ${t.directory}`)
  const has = (patterns: RegExp[]) => patterns.some(pattern =>
    pattern.test(haystack) || suffixes.some(suffix => pattern.test(suffix)),
  )

  if (has([/\b(ebook|ebooks|book|books|audiobook|epub|mobi|azw3|pdf|cbz|cbr)\b/, /^(epub|mobi|azw3|pdf|cbz|cbr)$/])) {
    return { icon: '📚', label: 'Ebook', color: '#a78bfa' }
  }
  if (has([/\b(s\d{1,2}e\d{1,3}|season|episode|hdtv|web-dl|webrip|tv)\b/])) {
    return { icon: '📺', label: 'TV', color: '#38bdf8' }
  }
  if (has([/\b(movie|movies|film|bluray|bdrip|dvdrip|x264|x265|h\.264|h\.265|2160p|1080p|720p)\b/, /^(mkv|mp4|avi|mov|wmv|m4v)$/])) {
    return { icon: '🎬', label: 'Video', color: '#60a5fa' }
  }
  if (has([/\b(music|album|discography|flac|mp3|aac|ogg|opus)\b/, /^(flac|mp3|aac|ogg|opus|wav|m4a)$/])) {
    return { icon: '🎵', label: 'Audio', color: '#34d399' }
  }
  if (has([/\b(iso|installer|image|linux|ubuntu|debian|archlinux|fedora)\b/, /^(iso|img|dmg)$/])) {
    return { icon: '💿', label: 'ISO/Image', color: 'var(--warning)' }
  }
  if (has([/\b(game|games|gog|steam|switch|ps4|ps5|xbox)\b/])) {
    return { icon: '🎮', label: 'Game', color: '#f472b6' }
  }
  if (has([/\b(app|software|source|code|github|windows|macos|linux)\b/, /^(exe|msi|pkg|deb|rpm|zip|tar|gz|xz|7z|rar)$/])) {
    return { icon: '🧩', label: 'Software/Archive', color: 'var(--muted)' }
  }
  return { icon: '📦', label: 'Other', color: 'var(--faint)' }
}

function fileSuffixes(text: string): string[] {
  const suffixes = new Set<string>()
  const matches = text.matchAll(/(?:^|[/\s._()[\]-])([a-z0-9][a-z0-9._ -]{0,180}\.([a-z0-9]{2,8}))(?:$|[/\s()[\]-])/gi)
  for (const match of matches) {
    const ext = match[2]?.toLowerCase()
    if (ext) suffixes.add(ext)
    const name = match[1]?.toLowerCase() ?? ''
    for (const nested of name.matchAll(/\.([a-z0-9]{2,8})(?=\.|$)/g)) {
      suffixes.add(nested[1])
    }
  }
  return [...suffixes]
}

function loadColumns(): ColKey[] {
  try {
    const raw = localStorage.getItem(COLUMN_STORAGE_KEY)
    if (!raw) return DEFAULT_VISIBLE
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return DEFAULT_VISIBLE
    const valid = new Set(COLS.map(c => c.key))
    const loaded = parsed.filter((key): key is ColKey => valid.has(key))
    return ['check', ...loaded.filter(key => key !== 'check')]
  } catch {
    return DEFAULT_VISIBLE
  }
}

const COLUMN_ORDER_KEY = 'tng.columnOrder.v1'
const COLUMN_WIDTHS_KEY = 'tng.columnWidths.v1'

function loadOrder(): ColKey[] {
  try {
    const raw = localStorage.getItem(COLUMN_ORDER_KEY)
    if (!raw) return DEFAULT_ORDER
    const parsed = JSON.parse(raw)
    if (!Array.isArray(parsed)) return DEFAULT_ORDER
    const valid = new Set(COLS.map(c => c.key))
    const loaded = parsed.filter((key): key is ColKey => valid.has(key))
    // Any column added since this order was saved (new release) is appended
    // at the end rather than silently disappearing from the table.
    const missing = DEFAULT_ORDER.filter(key => !loaded.includes(key))
    return [...loaded, ...missing]
  } catch {
    return DEFAULT_ORDER
  }
}

function loadWidths(): Partial<Record<ColKey, number>> {
  try {
    const raw = localStorage.getItem(COLUMN_WIDTHS_KEY)
    if (!raw) return {}
    const parsed = JSON.parse(raw) as Record<string, number>
    const valid = new Set(COLS.map(c => c.key))
    const out: Partial<Record<ColKey, number>> = {}
    for (const [key, value] of Object.entries(parsed)) {
      if (valid.has(key as ColKey) && typeof value === 'number' && value >= MIN_COL_WIDTH) {
        out[key as ColKey] = value
      }
    }
    return out
  } catch {
    return {}
  }
}

export function TorrentTable({
  torrents, total, selected, params, onSelect, onSelectAll, onSelectAllMatching, isSelectingAllMatching,
  onDetail, onContextMenu, onSort, onLoadMore, hasMore, isFetchingMore, detailHash, mediaInference,
}: Props) {
  const parentRef = useRef<HTMLDivElement>(null)
  const columnsRef = useRef<HTMLDivElement>(null)
  const loadMoreRef = useRef(false)
  const [visibleKeys, setVisibleKeys] = useState<ColKey[]>(loadColumns)
  const [columnsOpen, setColumnsOpen] = useState(false)
  const [order, setOrder] = useState<ColKey[]>(loadOrder)
  const [widths, setWidths] = useState<Partial<Record<ColKey, number>>>(loadWidths)
  const [dragKey, setDragKey] = useState<ColKey | null>(null)
  const resizeState = useRef<{ key: ColKey; startX: number; startWidth: number } | null>(null)

  const colByKey = useMemo(() => new Map(COLS.map(c => [c.key, c])), [])

  // Type inference off means every row would show the same "disabled"
  // placeholder -- not a useful column, so hide it (and its toggle in the
  // Columns menu below) rather than let the user pick a column that can
  // never show real data. The user's own visibleKeys preference for it is
  // left untouched, so it reappears automatically if they re-enable inference.
  const typeColumnUsable = mediaInference !== 'off'
  const toggleableCols = useMemo(
    () => COLS.filter(col => !col.required && (col.key !== 'kind' || typeColumnUsable)),
    [typeColumnUsable],
  )

  const visibleCols = useMemo(() => {
    const visible = new Set(visibleKeys)
    const ordered = order.map(key => colByKey.get(key)).filter((c): c is Col => Boolean(c))
    // 'check' always leads and 'actions' always trails regardless of drag
    // order - both are structural (selection checkbox, columns menu anchor)
    // rather than data the user would want to reposition.
    // The other sticky identity columns must lead as well; otherwise a user
    // could drag a non-sticky column ahead of Type/Name and leave the pinned
    // columns stranded in the middle of the horizontal scroll region.
    const middle = ordered.filter(c => !c.required && visible.has(c.key) && !STICKY_KEYS.includes(c.key))
    const check = colByKey.get('check')!
    const actions = colByKey.get('actions')!
    const sticky = STICKY_KEYS.slice(1)
      .map(key => colByKey.get(key))
      .filter((c): c is Col => Boolean(c && visible.has(c.key) && (c.key !== 'kind' || typeColumnUsable)))
    return [check, ...sticky, ...middle, actions]
  }, [visibleKeys, order, colByKey, typeColumnUsable])

  function resolvedWidth(col: Col): string {
    const override = widths[col.key]
    return override ? `${override}px` : col.width
  }
  const gridTemplate = visibleCols.map(resolvedWidth).join(' ')

  // Pixel offset of each sticky column's left edge, for position:sticky.
  const stickyLeft = useMemo(() => {
    const offsets: Partial<Record<ColKey, number>> = {}
    let x = TABLE_HORIZONTAL_PADDING
    for (const [index, col] of visibleCols.entries()) {
      if (STICKY_KEYS.includes(col.key)) {
        offsets[col.key] = x
      }
      const overriddenWidth = widths[col.key]
      const parsedWidth = Number.parseFloat(col.width)
      x += overriddenWidth ?? (Number.isFinite(parsedWidth) ? parsedWidth : 0)
      if (index < visibleCols.length - 1) x += TABLE_CELL_GAP
    }
    return offsets
  }, [visibleCols, widths])
  const lastStickyKey = [...visibleCols].reverse().find(c => STICKY_KEYS.includes(c.key))?.key

  function setColumnVisible(key: ColKey, visible: boolean) {
    setVisibleKeys(prev => {
      const next = visible
        ? [...prev, key]
        : prev.filter(k => k !== key)
      const deduped = COLS.map(c => c.key).filter(k => k === 'check' || next.includes(k))
      localStorage.setItem(COLUMN_STORAGE_KEY, JSON.stringify(deduped))
      return deduped
    })
  }

  function resetColumns() {
    localStorage.setItem(COLUMN_STORAGE_KEY, JSON.stringify(DEFAULT_VISIBLE))
    localStorage.removeItem(COLUMN_ORDER_KEY)
    localStorage.removeItem(COLUMN_WIDTHS_KEY)
    setVisibleKeys(DEFAULT_VISIBLE)
    setOrder(DEFAULT_ORDER)
    setWidths({})
  }

  function reorderColumn(source: ColKey, target: ColKey) {
    if (source === target) return
    setOrder(prev => {
      const next = prev.filter(k => k !== source)
      const targetIndex = next.indexOf(target)
      if (targetIndex === -1) return prev
      next.splice(targetIndex, 0, source)
      localStorage.setItem(COLUMN_ORDER_KEY, JSON.stringify(next))
      return next
    })
  }

  function beginResize(e: React.PointerEvent, col: Col) {
    e.preventDefault()
    e.stopPropagation()
    const startWidth = widths[col.key] ?? parseInt(col.width, 10) ?? 80
    resizeState.current = { key: col.key, startX: e.clientX, startWidth }
    function onMove(ev: PointerEvent) {
      if (!resizeState.current) return
      const delta = ev.clientX - resizeState.current.startX
      const next = Math.max(MIN_COL_WIDTH, Math.round(resizeState.current.startWidth + delta))
      setWidths(prev => ({ ...prev, [resizeState.current!.key]: next }))
    }
    function onUp() {
      if (resizeState.current) {
        setWidths(prev => {
          localStorage.setItem(COLUMN_WIDTHS_KEY, JSON.stringify(prev))
          return prev
        })
      }
      resizeState.current = null
      window.removeEventListener('pointermove', onMove)
      window.removeEventListener('pointerup', onUp)
    }
    window.addEventListener('pointermove', onMove)
    window.addEventListener('pointerup', onUp)
  }

  function useCompactColumns() {
    localStorage.setItem(COLUMN_STORAGE_KEY, JSON.stringify(COMPACT_VISIBLE))
    setVisibleKeys(COMPACT_VISIBLE)
  }

  const virtualizer = useVirtualizer({
    count: torrents.length,
    getScrollElement: () => parentRef.current,
    estimateSize: () => ROW_HEIGHT,
    overscan: 30,
  })

  // Trigger next page when within 500px of the bottom
  useEffect(() => {
    const el = parentRef.current
    if (!el) return
    function onScroll() {
      if (!el) return
      const near = el.scrollTop + el.clientHeight >= el.scrollHeight - 500
      if (near && hasMore && !isFetchingMore && !loadMoreRef.current) {
        loadMoreRef.current = true
        onLoadMore()
        setTimeout(() => { loadMoreRef.current = false }, 500)
      }
    }
    el.addEventListener('scroll', onScroll, { passive: true })
    return () => el.removeEventListener('scroll', onScroll)
  }, [hasMore, isFetchingMore, onLoadMore])

  useEffect(() => {
    if (!columnsOpen) return
    function onPointerDown(e: PointerEvent) {
      if (columnsRef.current?.contains(e.target as Node)) return
      setColumnsOpen(false)
    }
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') setColumnsOpen(false)
    }
    window.addEventListener('pointerdown', onPointerDown)
    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('pointerdown', onPointerDown)
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [columnsOpen])

  const items = virtualizer.getVirtualItems()
  const activeSort = params.sort ?? 'name'
  const activeDir = params.dir ?? 'asc'

  const allVisible = torrents.length > 0 && torrents.every(t => selected.has(t.hash))
  const someSelected = !allVisible && torrents.some(t => selected.has(t.hash))
  const hasFilters = Boolean(params.filter || params.status || params.category || params.tag || params.tracker || params.media_type)

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%', minWidth: 0 }}>
      {/* A single element must own both scroll axes: position:sticky for the
          pinned header/columns needs one unambiguous scrolling ancestor, and
          CSS auto-promotes overflow-x:visible to 'auto' whenever overflow-y
          isn't visible - splitting the axes across two nested elements (as
          this used to do) silently breaks sticky-left positioning. */}
      <div ref={parentRef} style={{ flex: 1, minHeight: 0, minWidth: 0, overflow: 'auto', position: 'relative' }}>
        {torrents.length === 0 && (
          <div style={{
            position: 'absolute', inset: 0, display: 'grid', placeItems: 'center',
            color: 'var(--faint)', fontSize: 13, textAlign: 'center', padding: 24, zIndex: 6,
          }}>
            <div className="tng-empty-state" data-filtered={hasFilters ? 'true' : 'false'} style={{
              border: '1px solid var(--border)', borderRadius: 8,
              background: 'var(--surface)', padding: '18px 22px', display: 'grid', gap: 6,
              maxWidth: 360,
            }}>
              <span style={{ color: 'var(--text)', fontWeight: 700 }}>No torrents match this view</span>
              <span>{hasFilters ? 'Clear filters or change the search text.' : 'Add a torrent to populate the table.'}</span>
            </div>
          </div>
        )}
        <div style={{ minWidth: TABLE_MIN_WIDTH }}>
          {/* Header */}
          <div style={{
            display: 'grid', gridTemplateColumns: gridTemplate, gap: `0 ${TABLE_CELL_GAP}px`,
            padding: `0 ${TABLE_HORIZONTAL_PADDING}px`, height: 32, alignItems: 'center',
            background: 'var(--table-head)', borderBottom: '1px solid var(--border-strong)',
            fontSize: 11, fontWeight: 600, color: 'var(--muted)',
            letterSpacing: '0.05em', textTransform: 'uppercase', fontVariantNumeric: 'tabular-nums',
            flexShrink: 0, userSelect: 'none', position: 'sticky', top: 0, zIndex: 5,
          }}>
            {/* Select-all checkbox */}
            <span style={STICKY_KEYS.includes('check') ? {
              position: 'sticky', left: stickyLeft.check ?? 0, zIndex: 3,
              background: 'var(--table-head)', display: 'flex', alignItems: 'center',
            } : undefined}>
              <input
                type="checkbox"
                aria-label={
                  allVisible ? 'Clear selection'
                    : isSelectingAllMatching ? 'Selecting all matching torrents…'
                      : `Select all ${total.toLocaleString()} torrents matching the current filter`
                }
                title={
                  allVisible ? 'Clear selection'
                    : total > torrents.length
                      ? `Select all ${total.toLocaleString()} torrents matching the current filter (not just the ${torrents.length.toLocaleString()} loaded so far)`
                      : 'Select all visible torrents'
                }
                checked={allVisible}
                disabled={isSelectingAllMatching}
                ref={el => { if (el) el.indeterminate = someSelected }}
                onChange={() => {
                  if (allVisible) {
                    onSelectAll([])
                  } else if (total > torrents.length && onSelectAllMatching) {
                    onSelectAllMatching()
                  } else {
                    onSelectAll(torrents.map(t => t.hash))
                  }
                }}
                style={{ accentColor: 'var(--accent)', cursor: isSelectingAllMatching ? 'wait' : 'pointer' }}
              />
            </span>
            {visibleCols.slice(1, -1).map(col => {
              const content = (
                <>
                  {col.label}
                  {col.sortKey === activeSort && (
                    <span style={{ fontSize: 9 }}>{activeDir === 'asc' ? '▲' : '▼'}</span>
                  )}
                </>
              )
              const sortKey = col.sortKey
              const isSticky = STICKY_KEYS.includes(col.key)
              const isLastSticky = col.key === lastStickyKey
              const draggable = !isSticky
              const inner = !sortKey ? (
                <span style={{ color: 'var(--muted)', display: 'flex', alignItems: 'center', gap: 3, overflow: 'hidden' }}>
                  {content}
                </span>
              ) : (
                <button
                  onClick={() => onSort(sortKey)}
                  title={`Sort by ${col.label}`}
                  aria-label={`Sort by ${col.label}`}
                  style={{
                    background: 'transparent', border: 0, padding: 0, margin: 0,
                    color: col.sortKey === activeSort ? 'var(--accent-text)' : 'var(--muted)',
                    display: 'flex', alignItems: 'center', gap: 3, overflow: 'hidden',
                    font: 'inherit', fontWeight: 600, textTransform: 'uppercase',
                    letterSpacing: '0.05em', cursor: 'pointer', width: '100%',
                  }}
                >
                  {content}
                </button>
              )
              return (
                <span
                  key={col.key}
                  draggable={draggable}
                  onDragStart={draggable ? (e: React.DragEvent) => { setDragKey(col.key); e.dataTransfer.effectAllowed = 'move' } : undefined}
                  onDragOver={draggable ? (e: React.DragEvent) => e.preventDefault() : undefined}
                  onDrop={draggable ? (e: React.DragEvent) => { e.preventDefault(); if (dragKey) reorderColumn(dragKey, col.key); setDragKey(null) } : undefined}
                  onDragEnd={draggable ? () => setDragKey(null) : undefined}
                  title={draggable ? `${col.label} - drag to reorder columns` : undefined}
                  style={{
                    position: 'relative', minWidth: 0, overflow: 'hidden',
                    cursor: draggable ? 'grab' : undefined,
                    opacity: dragKey === col.key ? 0.4 : 1,
                    outline: dragKey && dragKey !== col.key ? '1px dashed var(--border-strong)' : undefined,
                    ...(isSticky ? {
                      position: 'sticky', left: stickyLeft[col.key] ?? 0, zIndex: 3,
                      background: 'var(--table-head)',
                      boxShadow: isLastSticky ? '2px 0 0 var(--border-strong)' : undefined,
                    } : null),
                  }}
                >
                  {inner}
                  <span
                    onPointerDown={e => beginResize(e, col)}
                    title="Drag to resize column"
                    style={{
                      position: 'absolute', top: 0, right: -4, bottom: 0, width: 8,
                      cursor: 'col-resize', zIndex: 4,
                    }}
                  />
                </span>
              )
            })}
            <button
              onClick={() => setColumnsOpen(open => !open)}
              aria-expanded={columnsOpen}
              aria-haspopup="menu"
              aria-label="Choose visible table columns"
              title="Choose table columns"
              style={{
                position: 'absolute', right: 8, top: 5, background: 'var(--surface)',
                border: '1px solid var(--border-strong)', borderRadius: 4, color: 'var(--muted)',
                fontSize: 11, padding: '2px 7px', cursor: 'pointer',
              }}
            >
              Columns
            </button>
            {selected.size > 0 && (
              <span style={{
                position: 'absolute', right: 84, top: 6, height: 20,
                display: 'inline-flex', alignItems: 'center',
                border: '1px solid color-mix(in srgb, var(--accent) 35%, var(--border))',
                background: 'color-mix(in srgb, var(--accent) 12%, transparent)',
                color: 'var(--accent-text)', borderRadius: 999, padding: '0 7px',
                fontSize: 10, fontWeight: 800, textTransform: 'none', letterSpacing: 0,
              }}>
                {isSelectingAllMatching ? 'Selecting…' : `${selected.size.toLocaleString()} selected`}
              </span>
            )}
            {columnsOpen && (
              <div ref={columnsRef} role="menu" aria-label="Visible table columns" style={{
                position: 'absolute', right: 8, top: 30, zIndex: 20, width: 210,
                background: 'var(--panel)', border: '1px solid var(--border-strong)', borderRadius: 6,
                boxShadow: '0 18px 40px var(--shadow)', padding: 8,
                textTransform: 'none', letterSpacing: 0, fontWeight: 400,
              }}>
                <div style={{
                  display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8,
                  color: 'var(--faint)', fontSize: 11, margin: '2px 4px 7px',
                }}>
                  <span>Visible columns</span>
                  <span>{visibleCols.filter(col => !col.required).length}/{toggleableCols.length}</span>
                </div>
                {toggleableCols.map(col => (
                  <label key={col.key} className="tng-column-menu-item" data-active={visibleKeys.includes(col.key) ? 'true' : 'false'} style={{
                    display: 'flex', alignItems: 'center', gap: 8, color: 'var(--text)',
                    fontSize: 12, padding: '4px 3px', cursor: 'pointer',
                  }}>
                    <input
                      type="checkbox"
                      role="menuitemcheckbox"
                      aria-label={`Toggle ${col.label || col.key} column`}
                      aria-checked={visibleKeys.includes(col.key)}
                      checked={visibleKeys.includes(col.key)}
                      onChange={e => setColumnVisible(col.key, e.target.checked)}
                      style={{ accentColor: 'var(--accent)' }}
                    />
                    {col.label || col.key}
                  </label>
                ))}
                <button onClick={resetColumns} style={{
                  marginTop: 6, width: '100%', background: 'transparent',
                  border: '1px solid var(--border-strong)', borderRadius: 5, color: 'var(--muted)',
                  padding: '5px 8px', fontSize: 12, cursor: 'pointer',
                }}>Reset columns</button>
                <button onClick={useCompactColumns} style={{
                  marginTop: 6, width: '100%', background: 'transparent',
                  border: '1px solid var(--border-strong)', borderRadius: 5, color: 'var(--muted)',
                  padding: '5px 8px', fontSize: 12, cursor: 'pointer',
                }}>Compact preset</button>
                <button onClick={() => setColumnsOpen(false)} style={{
                  marginTop: 6, width: '100%', background: 'var(--surface-2)',
                  border: '1px solid var(--border-strong)', borderRadius: 5, color: 'var(--muted)',
                  padding: '5px 8px', fontSize: 12, cursor: 'pointer',
                }}>Done</button>
              </div>
            )}
          </div>

          {/* Scrollable body (rows render into parentRef above, not a nested scroller) */}
          <div style={{ position: 'relative' }}>
            <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {items.map(item => {
            const t = torrents[item.index]
            const { label, color } = statusLabel(t)
            const kind = mediaKind(t, mediaInference)
            const isSelected = selected.has(t.hash)
            const isDetail = detailHash === t.hash
            const accent = rowAccent(t)
            const cells: Record<ColKey, React.ReactNode> = {
              check: (
                <input
                  type="checkbox"
                  aria-label={`${isSelected ? 'Deselect' : 'Select'} ${t.name}`}
                  checked={isSelected}
                  onClick={e => e.stopPropagation()}
                  onChange={() => onSelect(t.hash)}
                  style={{ accentColor: 'var(--accent)', cursor: 'pointer' }}
                />
              ),
              kind: (
                <span title={kind.label} style={{
                  display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
                  width: 26, height: 22, color: kind.color, fontSize: 16,
                }}>{kind.icon}</span>
              ),
              name: (
                <span style={{ display: 'grid', gap: 1, minWidth: 0 }}>
                  <span
                    style={{
                      display: 'block',
                      overflow: 'hidden',
                      textOverflow: 'ellipsis',
                      whiteSpace: 'nowrap',
                      cursor: 'pointer',
                      color: 'var(--text)',
                      fontWeight: isSelected || isDetail ? 650 : 500,
                    }}
                    title={t.name}
                  >
                    {t.name}
                  </span>
                  {t.message && (
                    <span style={{
                      overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                      color: 'var(--warning)', fontSize: 10,
                    }} title={t.message}>{t.message}</span>
                  )}
                </span>
              ),
              status: (
                <span style={{
                  display: 'inline-flex', alignItems: 'center', justifyContent: 'center',
                  width: 'fit-content', maxWidth: '100%', minWidth: 42, height: 20,
                  padding: '0 7px', borderRadius: 4,
                  border: `1px solid color-mix(in srgb, ${color} 45%, var(--border))`,
                  background: `color-mix(in srgb, ${color} 12%, transparent)`,
                  color, fontSize: 10, fontWeight: 700,
                }}>
                  {label}
                </span>
              ),
              size: <span style={{ color: 'var(--muted)' }}>{fmtSize(t.size_bytes)}</span>,
              progress: <ProgressCell value={t.size_bytes ? Math.min(100, Math.max(0, (t.bytes_done / t.size_bytes) * 100)) : null} />,
              remaining: <span style={{ color: t.size_bytes <= 0 ? 'var(--faint)' : remainingBytes(t) ? 'var(--muted)' : 'var(--success)' }}>{t.size_bytes <= 0 ? '—' : remainingBytes(t) ? fmtSize(remainingBytes(t)) : 'Done'}</span>,
              eta: <span title={t.down_rate > 0 ? `${remainingBytes(t).toLocaleString()} bytes at ${fmtSpeed(t.down_rate)}` : undefined} style={{ color: t.complete ? 'var(--success)' : t.is_open ? 'var(--warning)' : 'var(--faint)' }}>{fmtEta(t)}</span>,
              down_rate: <span style={{ color: t.down_rate ? 'var(--accent)' : 'var(--faint)' }}>{fmtSpeed(t.down_rate)}</span>,
              up_rate: <span style={{ color: t.up_rate ? 'var(--success)' : 'var(--faint)' }}>{fmtSpeed(t.up_rate)}</span>,
              seeds: <span style={{ color: t.peers_complete ? 'var(--success)' : 'var(--faint)' }} title="Connected seeds">{t.peers_complete}</span>,
              peers: <span style={{ color: t.peers_connected ? 'var(--muted)' : 'var(--faint)' }} title="Connected peers">{t.peers_connected}</span>,
              ratio: <span style={{ color: t.ratio >= 1000 ? 'var(--success)' : 'var(--muted)' }}>{(t.ratio / 1000).toFixed(2)}</span>,
              added: <span style={{ color: 'var(--faint)' }}>{fmtDate(t.creation_date)}</span>,
              completed: <span style={{ color: 'var(--faint)' }}>{fmtDate(t.timestamp_finished)}</span>,
              downloaded: <span style={{ color: 'var(--muted)' }}>{fmtSize(t.down_total)}</span>,
              uploaded: <span style={{ color: 'var(--muted)' }}>{fmtSize(t.up_total)}</span>,
              priority: <span style={{ color: 'var(--faint)' }} title={`Priority value ${t.priority}`}>{priorityLabel(t.priority)}</span>,
              category: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{t.category || '—'}</span>,
              tags: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={t.tags}>{t.tags || '—'}</span>,
              tracker: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={t.tracker_url ? maskAnnounceUrl(t.tracker_url) : undefined}>{trackerHost(t.tracker_url)}</span>,
              path: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={t.base_path || t.directory}>{shortPath(t.base_path || t.directory)}</span>,
              actions: null,
            }
            const rowBg = isDetail || isSelected ? 'var(--selected)'
              : item.index % 2 === 0 ? 'var(--row)' : 'var(--row-alt)'
            return (
              <div
                key={t.hash}
                className="torrent-row"
                data-status={label.toLowerCase()}
                data-detail={isDetail ? 'true' : 'false'}
                title={`${isSelected ? 'Deselect' : 'Select'} ${t.name}`}
                onClick={() => onSelect(t.hash)}
                onContextMenu={e => {
                  e.preventDefault()
                  onContextMenu(t, e.clientX, e.clientY)
                }}
                onDoubleClick={() => onDetail(t.hash)}
                style={{
                  position: 'absolute', top: item.start, left: 0, right: 0,
                  height: ROW_HEIGHT, display: 'grid', gridTemplateColumns: gridTemplate,
                  gap: `0 ${TABLE_CELL_GAP}px`, padding: `0 ${TABLE_HORIZONTAL_PADDING}px`, alignItems: 'center',
                  cursor: 'pointer', fontSize: 13, fontVariantNumeric: 'tabular-nums',
                  background: rowBg,
                  borderBottom: '1px solid var(--border)',
                  borderLeft: isDetail ? '3px solid var(--accent)' : isSelected ? '3px solid color-mix(in srgb, var(--accent) 62%, transparent)' : `3px solid ${accent}`,
                }}
              >
                {visibleCols.map(col => {
                  const isSticky = STICKY_KEYS.includes(col.key)
                  const isLastSticky = col.key === lastStickyKey
                  return (
                    <span key={col.key} style={{
                      minWidth: 0, overflow: 'hidden',
                      ...(isSticky ? {
                        position: 'sticky', left: stickyLeft[col.key] ?? 0, zIndex: 2,
                        background: rowBg,
                        boxShadow: isLastSticky ? '2px 0 0 var(--border-strong)' : undefined,
                      } : null),
                    }}>{cells[col.key]}</span>
                  )
                })}
              </div>
            )
          })}
            </div>

            {/* Load-more sentinel */}
            {isFetchingMore && (
              <div style={{
                padding: '12px 0', display: 'grid', placeItems: 'center', gap: 6,
                fontSize: 11, color: 'var(--faint)',
              }}>
                <span className="tng-skeleton" style={{ width: 160, height: 8 }} />
                <span>Loading more torrents…</span>
              </div>
            )}
          </div>
        </div>
      </div>

      {hasMore && !isFetchingMore && (
        <button
          onClick={onLoadMore}
          title="Load the next page of torrents"
          style={{
          minHeight: 30, background: 'var(--table-head)', borderTop: '1px solid var(--border-strong)',
          borderLeft: 0, borderRight: 0, borderBottom: 0, width: '100%',
          display: 'flex', alignItems: 'center', justifyContent: 'center',
          fontSize: 11, color: 'var(--accent-text)', flexShrink: 0, cursor: 'pointer', gap: 6,
        }}>
          <span>Load more torrents</span>
          <span style={{ color: 'var(--faint)' }}>{torrents.length.toLocaleString()} / {total.toLocaleString()}</span>
        </button>
      )}
    </div>
  )
}

function ProgressCell({ value }: { value: number | null }) {
  if (value === null) {
    return <span style={{ color: 'var(--faint)' }}>—</span>
  }
  return (
    <span title={`${value.toFixed(1)}%`} style={{
      display: 'grid', gap: 3, width: '100%', minWidth: 0,
    }}>
      <span style={{ color: value >= 100 ? 'var(--success)' : 'var(--muted)', fontSize: 11, lineHeight: 1 }}>
        {value.toFixed(1)}%
      </span>
      <span style={{
        display: 'block', height: 3, borderRadius: 99, overflow: 'hidden',
        background: 'color-mix(in srgb, var(--border-strong) 72%, transparent)',
      }}>
        <span style={{
          display: 'block', height: '100%', width: `${value}%`,
          background: value >= 100 ? 'var(--success)' : 'var(--accent)',
        }} />
      </span>
    </span>
  )
}
