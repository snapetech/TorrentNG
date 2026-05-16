import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type ListParams, type SavedView } from '../api/client'

type Params = Omit<ListParams, 'limit' | 'offset'>

interface Props {
  params: Params
  total: number
  onChange: (p: Partial<ListParams>) => void
  onApply: (params: Params) => void
}

const STATUS_OPTIONS = [
  { value: '', label: 'All' },
  { value: 'downloading', label: 'Downloading' },
  { value: 'seeding', label: 'Seeding' },
  { value: 'completed', label: 'Completed' },
  { value: 'active', label: 'Active' },
  { value: 'stopped', label: 'Stopped' },
  { value: 'checking', label: 'Checking' },
  { value: 'error', label: 'Error' },
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

function isActive(params: Params, patch: Partial<ListParams>): boolean {
  return (params.status ?? '') === (patch.status ?? '')
    && (params.category ?? '') === (patch.category ?? '')
    && (params.tag ?? '') === (patch.tag ?? '')
    && (params.tracker ?? '') === (patch.tracker ?? '')
}

function trackerHost(url: string): string {
  try {
    return new URL(url).hostname
  } catch {
    return url
  }
}

export function TorrentSidebar({ params, total, onChange, onApply }: Props) {
  const qc = useQueryClient()
  const [viewName, setViewName] = useState('')

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
    staleTime: 30_000,
  })

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
    onChange({ status: undefined, category: undefined, tag: undefined, tracker: undefined, filter: undefined, offset: 0 })
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
            label={option.label}
            active={isActive(params, { status: option.value || undefined, category: undefined, tag: undefined, tracker: undefined })}
            count={option.value ? undefined : total}
            onClick={() => onChange({ status: option.value || undefined, category: undefined, tag: undefined, tracker: undefined, offset: 0 })}
          />
        ))}
      </Section>

      <Section title="Categories">
        <CountRow
          label="All categories"
          active={!params.category}
          count={total}
          onClick={() => onChange({ category: undefined, offset: 0 })}
        />
        {categories.slice(0, MAX_SECTION_ROWS).map(category => (
          <CountRow
            key={category.name}
            label={category.name}
            active={(params.category ?? '') === category.name}
            onClick={() => onChange({ category: category.name, tag: undefined, tracker: undefined, offset: 0 })}
          />
        ))}
        {categories.length > MAX_SECTION_ROWS && (
          <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} categories</div>
        )}
      </Section>

      {tags.length > 0 && (
        <Section title="Tags">
          <CountRow
            label="All tags"
            active={!params.tag}
            count={total}
            onClick={() => onChange({ tag: undefined, offset: 0 })}
          />
          {tags.slice(0, MAX_SECTION_ROWS).map(tag => (
            <CountRow
              key={tag}
              label={tag}
              active={(params.tag ?? '') === tag}
              onClick={() => onChange({ tag, category: undefined, tracker: undefined, offset: 0 })}
            />
          ))}
          {tags.length > MAX_SECTION_ROWS && (
            <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} tags</div>
          )}
        </Section>
      )}

      {trackerHealth && trackerHealth.trackers.length > 0 && (
        <Section title="Trackers">
          <CountRow
            label="All trackers"
            active={!params.tracker}
            count={total}
            onClick={() => onChange({ tracker: undefined, offset: 0 })}
          />
          {trackerHealth.trackers.slice(0, MAX_SECTION_ROWS).map(row => (
            <CountRow
              key={row.tracker}
              label={trackerHost(row.tracker)}
              active={(params.tracker ?? '') === row.tracker}
              count={row.torrent_count}
              onClick={() => onChange({ tracker: row.tracker, category: undefined, tag: undefined, offset: 0 })}
            />
          ))}
          {trackerHealth.trackers.length > MAX_SECTION_ROWS && (
            <div style={overflowNote}>Showing first {MAX_SECTION_ROWS.toLocaleString()} trackers</div>
          )}
        </Section>
      )}

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
              style={rowButtonStyle(false)}
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

      {(params.status || params.category || params.tag || params.tracker || params.filter) && (
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

function CountRow({ label, active, count, onClick }: {
  label: string
  active: boolean
  count?: number
  onClick: () => void
}) {
  return (
    <button onClick={onClick} style={rowButtonStyle(active)}>
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
    gridTemplateColumns: 'minmax(0, 1fr) auto',
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
