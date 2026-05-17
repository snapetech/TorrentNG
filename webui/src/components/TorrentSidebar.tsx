import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type ListParams, type SavedView } from '../api/client'
import type { MediaInferenceMode } from './AppearancePanel'

type Params = Omit<ListParams, 'limit' | 'offset'>

interface Props {
  params: Params
  total: number
  mediaInference: MediaInferenceMode
  onChange: (p: Partial<ListParams>) => void
  onApply: (params: Params) => void
}

const STATUS_OPTIONS = [
  { value: '', label: 'All', icon: '↔' },
  { value: 'downloading', label: 'Downloading', icon: '⇣' },
  { value: 'seeding', label: 'Seeding', icon: '⇡' },
  { value: 'completed', label: 'Completed', icon: '✓' },
  { value: 'running', label: 'Running', icon: '▶' },
  { value: 'stopped', label: 'Stopped', icon: '■' },
  { value: 'active', label: 'Active', icon: '⇅' },
  { value: 'inactive', label: 'Inactive', icon: '⇵' },
  { value: 'stalled', label: 'Stalled', icon: '↕' },
  { value: 'stalled_uploading', label: 'Stalled Uploading', icon: '⇡' },
  { value: 'stalled_downloading', label: 'Stalled Downloading', icon: '⇣' },
  { value: 'checking', label: 'Checking', icon: '↻' },
  { value: 'moving', label: 'Moving', icon: '⌖' },
  { value: 'error', label: 'Errored', icon: '!' },
]

const TYPE_OPTIONS = [
  { value: 'ebook', label: 'Ebooks', icon: '📚' },
  { value: 'tv', label: 'TV', icon: '📺' },
  { value: 'video', label: 'Video', icon: '🎬' },
  { value: 'audio', label: 'Audio', icon: '🎵' },
  { value: 'image', label: 'ISO / Images', icon: '💿' },
  { value: 'game', label: 'Games', icon: '🎮' },
  { value: 'software', label: 'Software / Archives', icon: '🧩' },
]

const SORT_OPTIONS = [
  { value: 'name', label: 'Name' },
  { value: 'added', label: 'Added' },
  { value: 'size', label: 'Size' },
  { value: 'progress', label: 'Progress' },
  { value: 'speed_down', label: 'Down speed' },
  { value: 'speed_up', label: 'Up speed' },
  { value: 'ratio', label: 'Ratio' },
]

const MAX_SECTION_ROWS = 80

function cleanParams(params: Params): Params {
  return Object.fromEntries(
    Object.entries(params).filter(([, value]) => value !== undefined && value !== ''),
  ) as Params
}

function trackerHost(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

function countValue(value?: number): number {
  return typeof value === 'number' && Number.isFinite(value) ? value : 0
}

export function TorrentSidebar({ params, total, mediaInference, onChange, onApply }: Props) {
  const qc = useQueryClient()
  const [viewName, setViewName] = useState('')
  const [trackerFilter, setTrackerFilter] = useState(params.tracker ?? '')
  const [viewsBusy, setViewsBusy] = useState<string | null>(null)
  const [viewsError, setViewsError] = useState<string | null>(null)

  const { data: categories = [] } = useQuery({
    queryKey: ['categories'],
    queryFn: api.categories.list,
    staleTime: 30_000,
  })

  const { data: tags = [] } = useQuery({
    queryKey: ['tags'],
    queryFn: api.tags.list,
    staleTime: 30_000,
  })

  const { data: views = [] } = useQuery({
    queryKey: ['saved-views'],
    queryFn: api.savedViews.list,
    staleTime: 30_000,
  })

  const { data: trackerHealth } = useQuery({
    queryKey: ['tracker-health'],
    queryFn: api.trackerHealth,
    staleTime: 5_000,
    refetchInterval: 10_000,
  })
  const { data: facets } = useQuery({
    queryKey: ['sidebar-facets'],
    queryFn: api.sidebarFacets,
    staleTime: 5_000,
    refetchInterval: 10_000,
  })

  useEffect(() => {
    setTrackerFilter(params.tracker ?? '')
  }, [params.tracker])

  async function saveView() {
    const name = viewName.trim()
    if (!name || viewsBusy) return
    const next: SavedView = { id: '', name, params: cleanParams(params) }
    setViewsBusy('__save__')
    setViewsError(null)
    try {
      await api.savedViews.save(next)
      setViewName('')
      qc.invalidateQueries({ queryKey: ['saved-views'] })
    } catch (err) {
      setViewsError(err instanceof Error ? err.message : 'Failed to save view.')
    } finally {
      setViewsBusy(null)
    }
  }

  async function removeView(id: string) {
    if (viewsBusy) return
    setViewsBusy(id)
    setViewsError(null)
    try {
      await api.savedViews.delete(id)
      qc.invalidateQueries({ queryKey: ['saved-views'] })
    } catch (err) {
      setViewsError(err instanceof Error ? err.message : 'Failed to delete view.')
    } finally {
      setViewsBusy(null)
    }
  }

  function clearFilters() {
    onChange({
      status: undefined,
      category: undefined,
      tag: undefined,
      tracker: undefined,
      media_type: undefined,
      filter: undefined,
      offset: 0,
    })
  }
  const activeFilterCount = [params.status, params.category, params.tag, params.tracker, params.media_type, params.filter]
    .filter(Boolean).length

  return (
    <aside className="torrent-sidebar" style={{
      width: 236, flexShrink: 0, background: 'var(--panel)', borderRight: '1px solid var(--border)',
      display: 'flex', flexDirection: 'column', overflowY: 'auto',
    }}>
      <div style={{
        position: 'sticky', top: 0, zIndex: 2, background: 'var(--panel)',
        borderBottom: '1px solid var(--border)', padding: '9px 10px',
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
          <span style={{ color: 'var(--text)', fontWeight: 700, fontSize: 13 }}>Library</span>
          <span style={{
            color: activeFilterCount ? 'var(--accent-text)' : 'var(--faint)',
            background: activeFilterCount ? 'var(--accent-soft)' : 'var(--surface)',
            border: '1px solid ' + (activeFilterCount ? 'var(--accent)' : 'var(--border)'),
            borderRadius: 999, padding: '1px 7px', fontSize: 10, fontWeight: 700,
          }}>
            {activeFilterCount ? `${activeFilterCount} active` : 'all'}
          </span>
        </div>
      </div>

      <Section title="State" summary={facets ? `${facets.status.all?.toLocaleString() ?? total.toLocaleString()} total` : undefined}>
        {STATUS_OPTIONS.map(option => (
          <CountRow
            key={option.value}
            icon={option.icon}
            label={option.label}
            active={(params.status ?? '') === option.value}
            count={facets?.status[option.value || 'all'] ?? (option.value ? undefined : total)}
            maxCount={facets?.status.all ?? total}
            onClick={() => onChange({ status: option.value || undefined, offset: 0 })}
          />
        ))}
      </Section>

      <Section title="Type" summary={mediaInference === 'off' ? 'manual' : 'inferred'}>
        <CountRow
          icon="◇"
          label="All types"
          active={!params.media_type}
          count={total}
          maxCount={total}
          onClick={() => onChange({ media_type: undefined, offset: 0 })}
        />
        {TYPE_OPTIONS.map(option => (
          <CountRow
            key={option.value}
            icon={option.icon}
            label={option.label}
            active={(params.media_type ?? '') === option.value}
            count={facets?.media_type[option.value]}
            maxCount={total}
            onClick={() => onChange({ media_type: option.value, offset: 0 })}
          />
        ))}
        {mediaInference === 'off' && (
          <div style={overflowNote}>Type filter still uses names, paths, and suffixes.</div>
        )}
      </Section>

      <Section title="Categories / Labels" summary={categories.length ? categories.length.toLocaleString() : undefined}>
        <CountRow
          icon="⌁"
          label="All categories"
          active={!params.category}
          count={categories.length}
          maxCount={Math.max(categories.length, ...categories.map(category => countValue(category.torrent_count)), 1)}
          onClick={() => onChange({ category: undefined, offset: 0 })}
        />
        {categories.slice(0, MAX_SECTION_ROWS).map(category => (
          <CountRow
            key={category.name}
            icon="⌁"
            label={category.name}
            active={(params.category ?? '') === category.name}
            count={category.torrent_count}
            maxCount={Math.max(...categories.map(category => countValue(category.torrent_count)), 1)}
            onClick={() => onChange({ category: category.name, offset: 0 })}
          />
        ))}
        {categories.length > MAX_SECTION_ROWS && (
          <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} categories</div>
        )}
      </Section>

      {tags.length > 0 && (
        <Section title="Tags" summary={tags.length.toLocaleString()}>
          <CountRow
            icon="#"
            label="All tags"
            active={!params.tag}
            count={tags.length}
            onClick={() => onChange({ tag: undefined, offset: 0 })}
          />
          {tags.slice(0, MAX_SECTION_ROWS).map(tag => (
            <CountRow
              key={tag}
              icon="#"
              label={tag}
              active={(params.tag ?? '') === tag}
              onClick={() => onChange({ tag, offset: 0 })}
            />
          ))}
          {tags.length > MAX_SECTION_ROWS && (
            <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} tags</div>
          )}
        </Section>
      )}

      <Section title="Trackers" summary={trackerHealth?.trackers.length.toLocaleString()}>
        <CountRow
          icon="☊"
          label="All trackers"
          active={!params.tracker}
          count={trackerHealth?.trackers.length ?? 0}
          maxCount={Math.max(trackerHealth?.trackers.length ?? 0, ...(trackerHealth?.trackers.map(row => row.torrent_count) ?? []), 1)}
          onClick={() => onChange({ tracker: undefined, offset: 0 })}
        />
        <form
          onSubmit={e => {
            e.preventDefault()
            onChange({ tracker: trackerFilter.trim() || undefined, offset: 0 })
          }}
          style={{ display: 'grid', gridTemplateColumns: '1fr 42px', gap: 5, margin: '5px 0 7px' }}
        >
          <input
            value={trackerFilter}
            onChange={e => setTrackerFilter(e.target.value)}
            placeholder="Tracker contains"
            style={inputStyle}
          />
          <button style={saveStyle(true)}>Go</button>
        </form>
        {trackerHealth?.trackers.slice(0, MAX_SECTION_ROWS).map(row => (
          <CountRow
            key={row.tracker}
            icon={row.error_count > 0 ? '!' : '☊'}
            label={trackerHost(row.tracker)}
            active={(params.tracker ?? '') === row.tracker}
            count={row.torrent_count}
            maxCount={Math.max(...trackerHealth.trackers.map(row => row.torrent_count), 1)}
            tone={row.error_count > 0 ? 'warn' : 'ok'}
            onClick={() => onChange({ tracker: row.tracker, offset: 0 })}
          />
        ))}
        {trackerHealth && trackerHealth.trackers.length === 0 && (
          <div style={overflowNote}>No cached tracker URLs yet.</div>
        )}
        {trackerHealth && trackerHealth.trackers.length > MAX_SECTION_ROWS && (
          <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} trackers</div>
        )}
      </Section>

      <Section title="Sort">
        <select
          value={params.sort ?? 'name'}
          onChange={e => onChange({ sort: e.target.value, offset: 0 })}
          style={selectStyle}
        >
          {SORT_OPTIONS.map(option => (
            <option key={option.value} value={option.value}>{option.label}</option>
          ))}
        </select>
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 6, marginTop: 6 }}>
          <ToggleButton active={(params.dir ?? 'asc') === 'asc'} onClick={() => onChange({ dir: 'asc', offset: 0 })}>Ascending</ToggleButton>
          <ToggleButton active={(params.dir ?? 'asc') === 'desc'} onClick={() => onChange({ dir: 'desc', offset: 0 })}>Descending</ToggleButton>
        </div>
      </Section>

      <Section title="Saved Views" summary={viewsBusy ? 'working' : views.length ? views.length.toLocaleString() : undefined}>
        {views.length === 0 && (
          <div style={emptySavedViewStyle}>
            Save the current filters as a named view.
          </div>
        )}
        {viewsError && <div style={sidebarNoticeStyle}>{viewsError}</div>}
        {views.map(view => (
          <div key={view.id} style={savedViewRowStyle}>
            <button
              onClick={() => onApply(view.params)}
              disabled={Boolean(viewsBusy)}
              title={JSON.stringify(view.params)}
              style={{ ...savedViewButtonStyle, opacity: viewsBusy ? 0.55 : 1, cursor: viewsBusy ? 'not-allowed' : 'pointer' }}
            >
              <span style={labelStyle}>{view.name}</span>
            </button>
            <button
              aria-label={`Delete ${view.name}`}
              onClick={() => removeView(view.id)}
              disabled={Boolean(viewsBusy)}
              title="Delete saved view"
              style={{ ...deleteStyle, opacity: viewsBusy ? 0.55 : 1, cursor: viewsBusy ? 'not-allowed' : 'pointer' }}
            >
              {viewsBusy === view.id ? '…' : 'x'}
            </button>
          </div>
        ))}
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 44px', gap: 6, marginTop: 7 }}>
          <input
            value={viewName}
            onChange={e => setViewName(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') saveView() }}
            disabled={Boolean(viewsBusy)}
            placeholder="Save view"
            style={inputStyle}
          />
          <button disabled={!viewName.trim() || Boolean(viewsBusy)} onClick={saveView} style={saveStyle(Boolean(viewName.trim()) && !viewsBusy)}>
            {viewsBusy === '__save__' ? '…' : 'Save'}
          </button>
        </div>
      </Section>

      {activeFilterCount > 0 && (
        <div style={{
          position: 'sticky', bottom: 0, background: 'var(--panel)',
          padding: '10px 12px 14px', borderTop: '1px solid var(--border)',
          boxShadow: '0 -10px 22px var(--shadow)',
        }}>
          <button onClick={clearFilters} style={{
            width: '100%', background: 'transparent', border: '1px solid var(--border-strong)', borderRadius: 5,
            color: 'var(--muted)', padding: '6px 8px', fontSize: 12, cursor: 'pointer',
          }}>
            Clear filters
          </button>
        </div>
      )}
    </aside>
  )
}

function CountRow({ icon, label, active, count, maxCount, tone, onClick }: {
  icon?: string
  label: string
  active: boolean
  count?: number
  maxCount?: number
  tone?: 'ok' | 'warn'
  onClick: () => void
}) {
  const pct = count !== undefined && maxCount ? Math.min(100, Math.max(4, (count / maxCount) * 100)) : 0
  return (
    <button onClick={onClick} style={rowButtonStyle(active)}>
      {count !== undefined && maxCount !== undefined && (
        <span aria-hidden="true" style={{
          position: 'absolute', left: 4, right: 4, bottom: 3, height: 2,
          borderRadius: 999, overflow: 'hidden',
          background: 'color-mix(in srgb, var(--border-strong) 36%, transparent)',
        }}>
          <span style={{
            display: 'block', width: `${pct}%`, height: '100%',
            background: active ? 'var(--accent)' : tone === 'warn' ? 'var(--warning)' : 'color-mix(in srgb, var(--accent) 52%, transparent)',
          }} />
        </span>
      )}
      {icon && <span style={iconStyle(active, tone)}>{icon}</span>}
      <span style={labelStyle}>{label}</span>
      <span style={{
        color: active ? 'var(--accent-text)' : tone === 'warn' ? 'var(--warning)' : 'var(--faint)',
        fontVariantNumeric: 'tabular-nums',
        border: count !== undefined ? '1px solid var(--border)' : undefined,
        borderRadius: 999,
        padding: count !== undefined ? '0 6px' : undefined,
        background: count !== undefined ? 'var(--surface)' : undefined,
      }}>
        {count === undefined ? '' : count.toLocaleString()}
      </span>
    </button>
  )
}

function Section({ title, summary, children }: { title: string; summary?: string; children: React.ReactNode }) {
  return (
    <section style={{ padding: '10px 10px 8px', borderBottom: '1px solid var(--border)' }}>
      <div style={{
        display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8,
        color: 'var(--faint)', fontSize: 11, fontWeight: 700, textTransform: 'uppercase',
        margin: '0 4px 7px',
      }}>
        <span>{title}</span>
        {summary && <span style={{ color: 'var(--faint)', fontWeight: 600, textTransform: 'none' }}>{summary}</span>}
      </div>
      {children}
    </section>
  )
}

function ToggleButton({ active, onClick, children }: {
  active: boolean
  onClick: () => void
  children: React.ReactNode
}) {
  return <button onClick={onClick} style={toggleStyle(active)}>{children}</button>
}

const labelStyle: React.CSSProperties = {
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
}

function iconStyle(active: boolean, tone?: 'ok' | 'warn'): React.CSSProperties {
  return {
    width: 18,
    color: active ? 'var(--accent-text)' : tone === 'warn' ? 'var(--warning)' : tone === 'ok' ? 'var(--success)' : 'var(--accent)',
    fontSize: 15,
    lineHeight: '14px',
    textAlign: 'center',
    fontWeight: 700,
  }
}

const selectStyle: React.CSSProperties = {
  width: '100%',
  background: 'var(--bg)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '6px 8px',
  fontSize: 12,
  outline: 'none',
}

const inputStyle: React.CSSProperties = {
  minWidth: 0,
  background: 'var(--bg)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '5px 7px',
  fontSize: 12,
  outline: 'none',
}

const deleteStyle: React.CSSProperties = {
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--faint)',
  cursor: 'pointer',
  fontSize: 12,
}

const savedViewButtonStyle: React.CSSProperties = {
  width: '100%',
  minWidth: 0,
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--muted)',
  padding: '5px 7px',
  fontSize: 12,
  cursor: 'pointer',
  display: 'block',
  textAlign: 'left',
}

const savedViewRowStyle: React.CSSProperties = {
  display: 'grid',
  gridTemplateColumns: '1fr 26px',
  gap: 4,
  marginBottom: 4,
}

const emptySavedViewStyle: React.CSSProperties = {
  color: 'var(--faint)',
  background: 'color-mix(in srgb, var(--surface) 72%, transparent)',
  border: '1px dashed var(--border-strong)',
  borderRadius: 6,
  fontSize: 11,
  lineHeight: 1.35,
  padding: '8px 9px',
  marginBottom: 7,
}

const sidebarNoticeStyle: React.CSSProperties = {
  color: 'var(--danger)',
  background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
  border: '1px solid color-mix(in srgb, var(--danger) 40%, var(--border))',
  borderRadius: 6,
  fontSize: 11,
  lineHeight: 1.35,
  padding: '7px 8px',
  marginBottom: 7,
  overflowWrap: 'anywhere',
}

const overflowNote: React.CSSProperties = {
  color: 'var(--faint)',
  fontSize: 11,
  padding: '5px 4px 2px',
}

function rowButtonStyle(active: boolean): React.CSSProperties {
  return {
    width: '100%',
    minWidth: 0,
    background: active ? 'var(--accent-soft)' : 'transparent',
    border: '1px solid ' + (active ? 'var(--accent)' : 'transparent'),
    borderRadius: 5,
    color: active ? 'var(--accent-text)' : 'var(--muted)',
    padding: '5px 7px',
    fontSize: 12,
    cursor: 'pointer',
    display: 'grid',
    gridTemplateColumns: 'auto minmax(0, 1fr) auto',
    gap: 8,
    alignItems: 'center',
    textAlign: 'left',
    position: 'relative',
    overflow: 'hidden',
  }
}

function toggleStyle(active: boolean): React.CSSProperties {
  return {
    background: active ? 'var(--accent-soft)' : 'var(--surface)',
    border: '1px solid ' + (active ? 'var(--accent)' : 'var(--border-strong)'),
    borderRadius: 5,
    color: active ? 'var(--accent-text)' : 'var(--muted)',
    padding: '5px 6px',
    fontSize: 11,
    cursor: 'pointer',
  }
}

function saveStyle(enabled: boolean): React.CSSProperties {
  return {
    background: enabled ? 'var(--accent-soft)' : 'var(--surface)',
    border: '1px solid ' + (enabled ? 'var(--accent)' : 'var(--border-strong)'),
    borderRadius: 5,
    color: enabled ? 'var(--accent-text)' : 'var(--faint)',
    padding: '5px 6px',
    fontSize: 11,
    cursor: enabled ? 'pointer' : 'not-allowed',
  }
}
