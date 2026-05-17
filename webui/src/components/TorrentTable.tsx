import { useRef, useEffect, useMemo, useState } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { TorrentSummary, ListParams } from '../api/client'
import type { MediaInferenceMode } from './AppearancePanel'

interface Props {
  torrents: TorrentSummary[]
  total: number
  selected: Set<string>
  params: ListParams
  onSelect: (hash: string) => void
  onSelectAll: (hashes: string[]) => void
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
const TABLE_MIN_WIDTH = 1080

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

function statusLabel(t: TorrentSummary): { label: string; color: string } {
  if (t.message && !t.is_active) return { label: 'Error', color: 'var(--danger)' }
  if (!t.is_open) return { label: 'Stopped', color: 'var(--faint)' }
  if (t.state === 2) return { label: 'Checking', color: 'var(--warning)' }
  if (t.complete && t.is_active) return { label: 'Seeding', color: 'var(--success)' }
  if (!t.complete && t.is_active) return { label: 'DL', color: 'var(--accent)' }
  return { label: 'Queued', color: 'var(--muted)' }
}

function rowAccent(t: TorrentSummary): string {
  if (t.message && !t.is_active) return 'var(--danger)'
  if (t.state === 2) return 'var(--warning)'
  if (!t.is_open) return 'var(--faint)'
  if (t.complete && t.is_active) return 'var(--success)'
  if (!t.complete && t.is_active) return 'var(--accent)'
  return 'transparent'
}

type ColKey =
  | 'check'
  | 'kind'
  | 'name'
  | 'status'
  | 'size'
  | 'progress'
  | 'down_rate'
  | 'up_rate'
  | 'ratio'
  | 'added'
  | 'category'
  | 'tags'
  | 'tracker'

interface Col { key: ColKey; label: string; width: string; sortKey?: string; required?: boolean }

const COLS: Col[] = [
  { key: 'check',     label: '',         width: '32px', required: true },
  { key: 'kind',      label: 'Type',     width: '52px' },
  { key: 'name',      label: 'Name',     width: 'minmax(200px, 1fr)', sortKey: 'name' },
  { key: 'status',    label: 'Status',   width: '72px' },
  { key: 'size',      label: 'Size',     width: '74px', sortKey: 'size' },
  { key: 'progress',  label: '%',        width: '56px', sortKey: 'progress' },
  { key: 'down_rate', label: '↓',        width: '74px', sortKey: 'speed_down' },
  { key: 'up_rate',   label: '↑',        width: '74px', sortKey: 'speed_up' },
  { key: 'ratio',     label: 'Ratio',    width: '54px', sortKey: 'ratio' },
  { key: 'added',     label: 'Added',    width: '80px', sortKey: 'added' },
  { key: 'category',  label: 'Category', width: '90px' },
  { key: 'tags',      label: 'Tags',     width: '96px' },
  { key: 'tracker',   label: 'Tracker',  width: '120px' },
]

const DEFAULT_VISIBLE: ColKey[] = [
  'check',
  'kind',
  'name',
  'status',
  'size',
  'progress',
  'down_rate',
  'up_rate',
  'ratio',
  'added',
  'category',
  'tags',
  'tracker',
]

const COMPACT_VISIBLE: ColKey[] = ['check', 'kind', 'name', 'status', 'progress', 'down_rate', 'up_rate', 'ratio']

const COLUMN_STORAGE_KEY = 'rtng.visibleColumns'

function fmtDate(ts: number): string {
  if (!ts) return '—'
  return new Date(ts * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: '2-digit' })
}

function trackerHost(url: string): string {
  if (!url) return '—'
  try {
    return new URL(url).hostname
  } catch {
    return url
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
  if (has([/\b(s\d{1,2}e\d{1,2}|season|episode|hdtv|web-dl|webrip|tv)\b/])) {
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

export function TorrentTable({
  torrents, total, selected, params, onSelect, onSelectAll, onDetail, onContextMenu, onSort,
  onLoadMore, hasMore, isFetchingMore, detailHash, mediaInference,
}: Props) {
  const parentRef = useRef<HTMLDivElement>(null)
  const columnsRef = useRef<HTMLDivElement>(null)
  const loadMoreRef = useRef(false)
  const [visibleKeys, setVisibleKeys] = useState<ColKey[]>(loadColumns)
  const [columnsOpen, setColumnsOpen] = useState(false)

  const visibleCols = useMemo(() => {
    const visible = new Set(visibleKeys)
    return COLS.filter(col => col.required || visible.has(col.key))
  }, [visibleKeys])
  const gridTemplate = visibleCols.map(c => c.width).join(' ')

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
    setVisibleKeys(DEFAULT_VISIBLE)
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
      <div style={{ flex: 1, minHeight: 0, minWidth: 0, overflowX: 'auto', overflowY: 'hidden' }}>
        <div style={{
          minWidth: TABLE_MIN_WIDTH, width: '100%', height: '100%',
          display: 'flex', flexDirection: 'column',
        }}>
          {/* Header */}
          <div style={{
            display: 'grid', gridTemplateColumns: gridTemplate, gap: '0 8px',
            padding: '0 96px 0 12px', height: 32, alignItems: 'center',
            background: 'var(--table-head)', borderBottom: '1px solid var(--border-strong)',
            fontSize: 11, fontWeight: 600, color: 'var(--muted)',
            letterSpacing: '0.05em', textTransform: 'uppercase',
            flexShrink: 0, userSelect: 'none', position: 'relative',
          }}>
            {/* Select-all checkbox */}
            <input
              type="checkbox"
              aria-label={allVisible ? 'Clear visible torrent selection' : 'Select all visible torrents'}
              title={allVisible ? 'Clear visible selection' : 'Select all visible torrents'}
              checked={allVisible}
              ref={el => { if (el) el.indeterminate = someSelected }}
              onChange={() => allVisible
                ? onSelectAll([])
                : onSelectAll(torrents.map(t => t.hash))
              }
              style={{ accentColor: 'var(--accent)', cursor: 'pointer' }}
            />
            {visibleCols.slice(1).map(col => {
              const content = (
                <>
                  {col.label}
                  {col.sortKey === activeSort && (
                    <span style={{ fontSize: 9 }}>{activeDir === 'asc' ? '▲' : '▼'}</span>
                  )}
                </>
              )
              const sortKey = col.sortKey
              if (!sortKey) {
                return (
                  <span
                    key={col.key}
                    style={{
                      color: 'var(--muted)', display: 'flex', alignItems: 'center', gap: 3,
                    }}
                  >
                    {content}
                  </span>
                )
              }
              return (
                <button
                  key={col.key}
                  onClick={() => onSort(sortKey)}
                  title={`Sort by ${col.label}`}
                  aria-label={`Sort by ${col.label}`}
                  aria-sort={col.sortKey === activeSort ? (activeDir === 'asc' ? 'ascending' : 'descending') : undefined}
                  style={{
                    background: 'transparent', border: 0, padding: 0, margin: 0,
                    color: col.sortKey === activeSort ? 'var(--accent-text)' : 'var(--muted)',
                    display: 'flex', alignItems: 'center', gap: 3,
                    font: 'inherit', fontWeight: 600, textTransform: 'uppercase',
                    letterSpacing: '0.05em', cursor: 'pointer',
                  }}
                >
                  {content}
                </button>
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
                  <span>{visibleCols.length - 1}/{COLS.length - 1}</span>
                </div>
                {COLS.filter(col => !col.required).map(col => (
                  <label key={col.key} style={{
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

          {/* Scrollable body */}
          <div ref={parentRef} style={{ flex: 1, minHeight: 0, overflowY: 'auto', overflowX: 'hidden', position: 'relative' }}>
            {torrents.length === 0 && (
              <div style={{
                position: 'absolute', inset: 0, display: 'grid', placeItems: 'center',
                color: 'var(--faint)', fontSize: 13, textAlign: 'center', padding: 24,
              }}>
                <div style={{
                  border: '1px solid var(--border)', borderRadius: 8,
                  background: 'var(--surface)', padding: '18px 22px', display: 'grid', gap: 6,
                  maxWidth: 360,
                }}>
                  <span style={{ color: 'var(--text)', fontWeight: 700 }}>No torrents match this view</span>
                  <span>{hasFilters ? 'Clear filters or change the search text.' : 'Add a torrent to populate the table.'}</span>
                </div>
              </div>
            )}
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
              down_rate: <span style={{ color: t.down_rate ? 'var(--accent)' : 'var(--faint)' }}>{fmtSpeed(t.down_rate)}</span>,
              up_rate: <span style={{ color: t.up_rate ? 'var(--success)' : 'var(--faint)' }}>{fmtSpeed(t.up_rate)}</span>,
              ratio: <span style={{ color: t.ratio >= 1000 ? 'var(--success)' : 'var(--muted)' }}>{(t.ratio / 1000).toFixed(2)}</span>,
              added: <span style={{ color: 'var(--faint)' }}>{fmtDate(t.creation_date)}</span>,
              category: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{t.category || '—'}</span>,
              tags: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={t.tags}>{t.tags || '—'}</span>,
              tracker: <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={t.tracker_url}>{trackerHost(t.tracker_url)}</span>,
            }
            return (
              <div
                key={t.hash}
                className="torrent-row"
                role="button"
                tabIndex={0}
                aria-selected={isSelected}
                aria-label={`${isSelected ? 'Deselect' : 'Select'} torrent ${t.name}`}
                title={`${isSelected ? 'Deselect' : 'Select'} ${t.name}`}
                onClick={() => onSelect(t.hash)}
                onKeyDown={e => {
                  if (e.key === 'Enter' || e.key === ' ') {
                    e.preventDefault()
                    onSelect(t.hash)
                  }
                }}
                onContextMenu={e => {
                  e.preventDefault()
                  onContextMenu(t, e.clientX, e.clientY)
                }}
                onDoubleClick={() => onDetail(t.hash)}
                style={{
                  position: 'absolute', top: item.start, left: 0, right: 0,
                  height: ROW_HEIGHT, display: 'grid', gridTemplateColumns: gridTemplate,
                  gap: '0 8px', padding: '0 12px', alignItems: 'center',
                  cursor: 'pointer', fontSize: 13,
                  background: isDetail || isSelected ? 'var(--selected)'
                    : item.index % 2 === 0 ? 'var(--row)' : 'var(--row-alt)',
                  borderBottom: '1px solid var(--border)',
                  borderLeft: isDetail ? '3px solid var(--accent)' : isSelected ? '3px solid color-mix(in srgb, var(--accent) 62%, transparent)' : `3px solid ${accent}`,
                }}
              >
                {visibleCols.map(col => (
                  <span key={col.key} style={{ minWidth: 0, overflow: 'hidden' }}>{cells[col.key]}</span>
                ))}
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
                <span className="rtng-skeleton" style={{ width: 160, height: 8 }} />
                <span>Loading more torrents…</span>
              </div>
            )}
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
      </div>
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
