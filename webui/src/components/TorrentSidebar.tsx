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

export function TorrentSidebar({ params, total, mediaInference, onChange, onApply }: Props) {
  const qc = useQueryClient()
  const [viewName, setViewName] = useState('')
  const [trackerFilter, setTrackerFilter] = useState(params.tracker ?? '')

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
    if (!name) return
    const next: SavedView = { id: '', name, params: cleanParams(params) }
    await api.savedViews.save(next)
    setViewName('')
    qc.invalidateQueries({ queryKey: ['saved-views'] })
  }

  async function removeView(id: string) {
    await api.savedViews.delete(id)
    qc.invalidateQueries({ queryKey: ['saved-views'] })
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

  return (
    <aside style={{
      width: 236, flexShrink: 0, background: '#0f141d', borderRight: '1px solid #1e2433',
      display: 'flex', flexDirection: 'column', overflowY: 'auto',
    }}>
      <Section title="State">
        {STATUS_OPTIONS.map(option => (
          <CountRow
            key={option.value}
            icon={option.icon}
            label={option.label}
            active={(params.status ?? '') === option.value}
            count={facets?.status[option.value || 'all'] ?? (option.value ? undefined : total)}
            onClick={() => onChange({ status: option.value || undefined, offset: 0 })}
          />
        ))}
      </Section>

      <Section title="Type">
        <CountRow
          icon="◇"
          label="All types"
          active={!params.media_type}
          count={total}
          onClick={() => onChange({ media_type: undefined, offset: 0 })}
        />
        {TYPE_OPTIONS.map(option => (
          <CountRow
            key={option.value}
            icon={option.icon}
            label={option.label}
            active={(params.media_type ?? '') === option.value}
            count={facets?.media_type[option.value]}
            onClick={() => onChange({ media_type: option.value, offset: 0 })}
          />
        ))}
        {mediaInference === 'off' && (
          <div style={overflowNote}>Type filter still uses names, paths, and suffixes.</div>
        )}
      </Section>

      <Section title="Categories / Labels">
        <CountRow
          icon="⌁"
          label="All categories"
          active={!params.category}
          count={categories.length}
          onClick={() => onChange({ category: undefined, offset: 0 })}
        />
        {categories.slice(0, MAX_SECTION_ROWS).map(category => (
          <CountRow
            key={category.name}
            icon="⌁"
            label={category.name}
            active={(params.category ?? '') === category.name}
            count={category.torrent_count}
            onClick={() => onChange({ category: category.name, offset: 0 })}
          />
        ))}
        {categories.length > MAX_SECTION_ROWS && (
          <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} categories</div>
        )}
      </Section>

      {tags.length > 0 && (
        <Section title="Tags">
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

      <Section title="Trackers">
        <CountRow
          icon="☊"
          label="All trackers"
          active={!params.tracker}
          count={trackerHealth?.trackers.length ?? 0}
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

      <Section title="Saved Views">
        {views.length === 0 && <div style={{ color: '#475569', fontSize: 12, padding: '2px 4px 6px' }}>No saved views</div>}
        {views.map(view => (
          <div key={view.id} style={{ display: 'grid', gridTemplateColumns: '1fr 26px', gap: 4, marginBottom: 4 }}>
            <button
              onClick={() => onApply(view.params)}
              title={JSON.stringify(view.params)}
              style={savedViewButtonStyle}
            >
              <span style={labelStyle}>{view.name}</span>
            </button>
            <button onClick={() => removeView(view.id)} title="Delete saved view" style={deleteStyle}>x</button>
          </div>
        ))}
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 44px', gap: 6, marginTop: 6 }}>
          <input
            value={viewName}
            onChange={e => setViewName(e.target.value)}
            onKeyDown={e => { if (e.key === 'Enter') saveView() }}
            placeholder="Save view"
            style={inputStyle}
          />
          <button disabled={!viewName.trim()} onClick={saveView} style={saveStyle(Boolean(viewName.trim()))}>Save</button>
        </div>
      </Section>

      {(params.status || params.category || params.tag || params.tracker || params.media_type || params.filter) && (
        <div style={{ padding: '10px 12px 14px', borderTop: '1px solid #1e2433' }}>
          <button onClick={clearFilters} style={{
            width: '100%', background: 'transparent', border: '1px solid #334155', borderRadius: 5,
            color: '#94a3b8', padding: '6px 8px', fontSize: 12, cursor: 'pointer',
          }}>
            Clear filters
          </button>
        </div>
      )}
    </aside>
  )
}

function CountRow({ icon, label, active, count, onClick }: {
  icon?: string
  label: string
  active: boolean
  count?: number
  onClick: () => void
}) {
  return (
    <button onClick={onClick} style={rowButtonStyle(active)}>
      {icon && <span style={iconStyle(active)}>{icon}</span>}
      <span style={labelStyle}>{label}</span>
      <span style={{ color: active ? '#bfdbfe' : '#64748b', fontVariantNumeric: 'tabular-nums' }}>
        {count === undefined ? '' : count.toLocaleString()}
      </span>
    </button>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <section style={{ padding: '10px 10px 8px', borderBottom: '1px solid #1e2433' }}>
      <div style={{
        color: '#64748b', fontSize: 11, fontWeight: 700, textTransform: 'uppercase',
        margin: '0 4px 7px',
      }}>{title}</div>
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

function iconStyle(active: boolean): React.CSSProperties {
  return {
    width: 18,
    color: active ? '#bfdbfe' : '#60a5fa',
    fontSize: 15,
    lineHeight: '14px',
    textAlign: 'center',
    fontWeight: 700,
  }
}

const selectStyle: React.CSSProperties = {
  width: '100%',
  background: '#0d1117',
  border: '1px solid #334155',
  borderRadius: 5,
  color: '#cbd5e1',
  padding: '6px 8px',
  fontSize: 12,
  outline: 'none',
}

const inputStyle: React.CSSProperties = {
  minWidth: 0,
  background: '#0d1117',
  border: '1px solid #334155',
  borderRadius: 5,
  color: '#cbd5e1',
  padding: '5px 7px',
  fontSize: 12,
  outline: 'none',
}

const deleteStyle: React.CSSProperties = {
  background: 'transparent',
  border: '1px solid #334155',
  borderRadius: 5,
  color: '#64748b',
  cursor: 'pointer',
  fontSize: 12,
}

const savedViewButtonStyle: React.CSSProperties = {
  width: '100%',
  minWidth: 0,
  background: 'transparent',
  border: '1px solid transparent',
  borderRadius: 5,
  color: '#94a3b8',
  padding: '5px 7px',
  fontSize: 12,
  cursor: 'pointer',
  display: 'block',
  textAlign: 'left',
}

const overflowNote: React.CSSProperties = {
  color: '#475569',
  fontSize: 11,
  padding: '5px 4px 2px',
}

function rowButtonStyle(active: boolean): React.CSSProperties {
  return {
    width: '100%',
    minWidth: 0,
    background: active ? '#1e3a5f' : 'transparent',
    border: '1px solid ' + (active ? '#3b82f6' : 'transparent'),
    borderRadius: 5,
    color: active ? '#dbeafe' : '#94a3b8',
    padding: '5px 7px',
    fontSize: 12,
    cursor: 'pointer',
    display: 'grid',
    gridTemplateColumns: 'auto minmax(0, 1fr) auto',
    gap: 8,
    alignItems: 'center',
    textAlign: 'left',
  }
}

function toggleStyle(active: boolean): React.CSSProperties {
  return {
    background: active ? '#1e3a5f' : '#111827',
    border: '1px solid ' + (active ? '#3b82f6' : '#334155'),
    borderRadius: 5,
    color: active ? '#bfdbfe' : '#94a3b8',
    padding: '5px 6px',
    fontSize: 11,
    cursor: 'pointer',
  }
}

function saveStyle(enabled: boolean): React.CSSProperties {
  return {
    background: enabled ? '#1e3a5f' : '#111827',
    border: '1px solid ' + (enabled ? '#3b82f6' : '#334155'),
    borderRadius: 5,
    color: enabled ? '#bfdbfe' : '#475569',
    padding: '5px 6px',
    fontSize: 11,
    cursor: enabled ? 'pointer' : 'not-allowed',
  }
}
