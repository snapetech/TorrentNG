import { useRef, useEffect } from 'react'
import { useVirtualizer } from '@tanstack/react-virtual'
import type { TorrentSummary, ListParams } from '../api/client'

interface Props {
  torrents: TorrentSummary[]
  total: number
  selected: Set<string>
  params: ListParams
  onSelect: (hash: string) => void
  onSelectAll: (hashes: string[]) => void
  onDetail: (hash: string | null) => void
  onSort: (sort: string) => void
  onLoadMore: () => void
  hasMore: boolean
  isFetchingMore: boolean
  detailHash: string | null
}

const ROW_HEIGHT = 36

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

function fmtProgress(t: TorrentSummary): string {
  if (!t.size_bytes) return '—'
  return ((t.bytes_done / t.size_bytes) * 100).toFixed(1) + '%'
}

function statusLabel(t: TorrentSummary): { label: string; color: string } {
  if (t.message && !t.is_active) return { label: 'Error', color: '#ef4444' }
  if (!t.is_open) return { label: 'Stopped', color: '#64748b' }
  if (t.state === 2) return { label: 'Checking', color: '#f59e0b' }
  if (t.complete && t.is_active) return { label: 'Seeding', color: '#22c55e' }
  if (!t.complete && t.is_active) return { label: 'DL', color: '#3b82f6' }
  return { label: 'Queued', color: '#94a3b8' }
}

interface Col { key: string; label: string; width: string; sortKey?: string }

const COLS: Col[] = [
  { key: 'check',     label: '',         width: '32px' },
  { key: 'name',      label: 'Name',     width: 'minmax(200px, 1fr)', sortKey: 'name' },
  { key: 'status',    label: 'Status',   width: '72px' },
  { key: 'size',      label: 'Size',     width: '74px', sortKey: 'size' },
  { key: 'progress',  label: '%',        width: '56px', sortKey: 'progress' },
  { key: 'down_rate', label: '↓',        width: '74px', sortKey: 'speed_down' },
  { key: 'up_rate',   label: '↑',        width: '74px', sortKey: 'speed_up' },
  { key: 'ratio',     label: 'Ratio',    width: '54px', sortKey: 'ratio' },
  { key: 'added',     label: 'Added',    width: '80px', sortKey: 'added' },
  { key: 'category',  label: 'Category', width: '90px' },
]

const gridTemplate = COLS.map(c => c.width).join(' ')

function fmtDate(ts: number): string {
  if (!ts) return '—'
  return new Date(ts * 1000).toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: '2-digit' })
}

export function TorrentTable({
  torrents, total, selected, params, onSelect, onSelectAll, onDetail, onSort,
  onLoadMore, hasMore, isFetchingMore, detailHash,
}: Props) {
  const parentRef = useRef<HTMLDivElement>(null)
  const loadMoreRef = useRef(false)

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

  const items = virtualizer.getVirtualItems()
  const activeSort = params.sort ?? 'name'
  const activeDir = params.dir ?? 'asc'

  const allVisible = torrents.length > 0 && torrents.every(t => selected.has(t.hash))
  const someSelected = !allVisible && torrents.some(t => selected.has(t.hash))

  return (
    <div style={{ display: 'flex', flexDirection: 'column', height: '100%' }}>
      {/* Header */}
      <div style={{
        display: 'grid', gridTemplateColumns: gridTemplate, gap: '0 8px',
        padding: '0 12px', height: 32, alignItems: 'center',
        background: '#1e2433', borderBottom: '1px solid #2d3748',
        fontSize: 11, fontWeight: 600, color: '#94a3b8',
        letterSpacing: '0.05em', textTransform: 'uppercase',
        flexShrink: 0, userSelect: 'none',
      }}>
        {/* Select-all checkbox */}
        <input
          type="checkbox"
          checked={allVisible}
          ref={el => { if (el) el.indeterminate = someSelected }}
          onChange={() => allVisible
            ? onSelectAll([])
            : onSelectAll(torrents.map(t => t.hash))
          }
          style={{ accentColor: '#3b82f6', cursor: 'pointer' }}
        />
        {COLS.slice(1).map(col => (
          <span
            key={col.key}
            onClick={() => col.sortKey && onSort(col.sortKey)}
            style={{
              cursor: col.sortKey ? 'pointer' : 'default',
              color: col.sortKey === activeSort ? '#93c5fd' : '#94a3b8',
              display: 'flex', alignItems: 'center', gap: 3,
            }}
          >
            {col.label}
            {col.sortKey === activeSort && (
              <span style={{ fontSize: 9 }}>{activeDir === 'asc' ? '▲' : '▼'}</span>
            )}
          </span>
        ))}
      </div>

      {/* Scrollable body */}
      <div ref={parentRef} style={{ flex: 1, overflowY: 'auto', position: 'relative' }}>
        <div style={{ height: virtualizer.getTotalSize(), position: 'relative' }}>
          {items.map(item => {
            const t = torrents[item.index]
            const { label, color } = statusLabel(t)
            const isSelected = selected.has(t.hash)
            const isDetail = detailHash === t.hash
            return (
              <div
                key={t.hash}
                style={{
                  position: 'absolute', top: item.start, left: 0, right: 0,
                  height: ROW_HEIGHT, display: 'grid', gridTemplateColumns: gridTemplate,
                  gap: '0 8px', padding: '0 12px', alignItems: 'center',
                  cursor: 'default', fontSize: 13,
                  background: isDetail ? '#152040' : isSelected ? '#14203a'
                    : item.index % 2 === 0 ? '#131820' : '#151c27',
                  borderBottom: '1px solid #1e2433',
                  borderLeft: isDetail ? '2px solid #3b82f6' : '2px solid transparent',
                }}
              >
                <input
                  type="checkbox"
                  checked={isSelected}
                  onChange={() => onSelect(t.hash)}
                  style={{ accentColor: '#3b82f6', cursor: 'pointer' }}
                />
                <span
                  style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', cursor: 'pointer', color: '#e2e8f0' }}
                  title={t.name}
                  onClick={() => onDetail(isDetail ? null : t.hash)}
                >
                  {t.name}
                </span>
                <span style={{ color, fontSize: 11, fontWeight: 600 }}>{label}</span>
                <span style={{ color: '#94a3b8' }}>{fmtSize(t.size_bytes)}</span>
                <span style={{ color: '#94a3b8' }}>{fmtProgress(t)}</span>
                <span style={{ color: t.down_rate ? '#3b82f6' : '#475569' }}>{fmtSpeed(t.down_rate)}</span>
                <span style={{ color: t.up_rate   ? '#22c55e' : '#475569' }}>{fmtSpeed(t.up_rate)}</span>
                <span style={{ color: '#94a3b8' }}>{(t.ratio / 1000).toFixed(2)}</span>
                <span style={{ color: '#64748b' }}>{fmtDate(t.creation_date)}</span>
                <span style={{ color: '#64748b', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                  {t.category || '—'}
                </span>
              </div>
            )
          })}
        </div>

        {/* Load-more sentinel */}
        {isFetchingMore && (
          <div style={{ padding: '12px 0', textAlign: 'center', fontSize: 11, color: '#475569' }}>
            Loading more…
          </div>
        )}
      </div>

      {/* Footer */}
      <div style={{
        height: 28, background: '#1e2433', borderTop: '1px solid #2d3748',
        display: 'flex', alignItems: 'center', padding: '0 12px',
        fontSize: 11, color: '#64748b', flexShrink: 0, gap: 16,
      }}>
        <span>{torrents.length.toLocaleString()} / {total.toLocaleString()} torrent{total !== 1 ? 's' : ''}</span>
        {selected.size > 0 && <span style={{ color: '#3b82f6' }}>{selected.size} selected</span>}
        {hasMore && !isFetchingMore && (
          <span style={{ color: '#334155' }}>↓ scroll for more</span>
        )}
      </div>
    </div>
  )
}
