import { useEffect, useState } from 'react'
import type { ListParams } from '../api/client'

interface Props {
  params: ListParams
  onChange: (p: Partial<ListParams>) => void
}

export function FilterBar({ params, onChange }: Props) {
  const [search, setSearch] = useState(params.filter ?? '')
  const hasSidebarFilters = Boolean(params.status || params.category || params.tag || params.tracker || params.media_type)
  const activeCount = [params.filter || search, params.status, params.category, params.tag, params.tracker, params.media_type].filter(Boolean).length
  const sortLabel = `${params.sort ?? 'name'} ${params.dir === 'desc' ? 'desc' : 'asc'}`
  const chips = [
    (params.filter || search) && ['Search', params.filter || search, 'filter'],
    params.status && ['State', params.status, 'status'],
    params.media_type && ['Type', params.media_type, 'media_type'],
    params.category && ['Category', params.category, 'category'],
    params.tag && ['Tag', params.tag, 'tag'],
    params.tracker && ['Tracker', params.tracker, 'tracker'],
  ].filter(Boolean) as Array<[string, string, keyof ListParams]>

  useEffect(() => {
    const t = setTimeout(() => onChange({ filter: search, offset: 0 }), 200)
    return () => clearTimeout(t)
  }, [search, onChange])

  return (
    <div className="tng-filterbar" style={{
      display: 'flex',
      gap: 8,
      padding: '7px 12px',
      background: 'var(--surface-2)',
      borderBottom: '1px solid var(--border-strong)',
      alignItems: 'center',
      flexWrap: 'wrap',
    }}>
      <label className="tng-filterbar-search" style={{
        flex: '1 1 220px', minWidth: 0, display: 'flex', alignItems: 'center', gap: 7,
        background: 'var(--bg)', border: '1px solid ' + (search ? 'var(--accent)' : 'var(--border-strong)'), borderRadius: 6,
        padding: '0 9px',
        boxShadow: search ? '0 0 0 2px color-mix(in srgb, var(--accent) 14%, transparent)' : undefined,
      }}>
        <span style={{ color: 'var(--faint)', fontSize: 12 }}>⌕</span>
        <input
          aria-label="Search torrents"
          type="search"
          placeholder="Search torrents"
          value={search}
          onChange={e => setSearch(e.target.value)}
          style={{
            flex: 1,
            minWidth: 0,
            background: 'transparent',
            border: 0,
            color: 'var(--text)',
            padding: '5px 0',
            fontSize: 13,
            outline: 'none',
          }}
        />
        {search && (
          <span style={{
            color: 'var(--accent-text)', background: 'var(--accent-soft)',
            border: '1px solid color-mix(in srgb, var(--accent) 45%, var(--border))',
            borderRadius: 999, padding: '1px 6px', fontSize: 10, fontWeight: 800,
            whiteSpace: 'nowrap',
          }}>
            live
          </span>
        )}
      </label>

      {(params.filter || search) && (
        <button
          className="tng-filterbar-button"
          type="button"
          aria-label="Clear torrent search"
          onClick={() => {
            setSearch('')
            onChange({ filter: undefined, offset: 0 })
          }}
          style={{
            background: 'none',
            border: '1px solid var(--border-strong)',
            borderRadius: 5,
            color: 'var(--faint)',
            padding: '3px 8px',
            fontSize: 11,
            cursor: 'pointer',
          }}
        >
          Clear search
        </button>
      )}
      {hasSidebarFilters && (
        <button
          className="tng-filterbar-button"
          type="button"
          aria-label="Clear sidebar filters"
          onClick={() => onChange({
            status: undefined,
            category: undefined,
            tag: undefined,
            tracker: undefined,
            media_type: undefined,
            offset: 0,
          })}
          title="Clear sidebar filters"
          style={{
            background: 'none',
            border: '1px solid var(--border-strong)',
            borderRadius: 5,
            color: 'var(--muted)',
            padding: '3px 8px',
            fontSize: 11,
            cursor: 'pointer',
          }}
        >
          Clear filters
        </button>
      )}
      {hasSidebarFilters && (
        <span className="tng-filterbar-count" style={{
          color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
          borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 800, whiteSpace: 'nowrap',
        }}>
          {activeCount.toLocaleString()} active
        </span>
      )}
      {(chips.length > 0 || params.sort) && (
        <div className="tng-filterbar-chips" style={{ display: 'flex', gap: 5, flexWrap: 'wrap', minWidth: 0 }}>
          <span className="tng-filter-chip tng-filter-chip-muted" title={`Sorted by ${sortLabel}`} style={{
            display: 'inline-flex', alignItems: 'center', gap: 4,
            maxWidth: 190, border: '1px solid var(--border)', borderRadius: 999,
            background: 'var(--surface)', color: 'var(--muted)', padding: '2px 7px',
            fontSize: 11,
          }}>
            <span style={{ color: 'var(--faint)' }}>Sort</span>
            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{sortLabel}</span>
          </span>
          {chips.map(([label, value, key]) => (
            <span key={`${label}:${value}`} className="tng-filter-chip" title={value} style={{
              display: 'inline-flex', alignItems: 'center', gap: 4,
              maxWidth: 190, border: '1px solid var(--border)', borderRadius: 999,
              background: 'var(--surface)', color: 'var(--muted)', padding: '2px 7px',
              fontSize: 11,
            }}>
              <span style={{ color: 'var(--faint)' }}>{label}</span>
              <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{value}</span>
              <button
                type="button"
                aria-label={`Clear ${label} filter`}
                onClick={() => {
                  if (key === 'filter') setSearch('')
                  onChange({ [key]: undefined, offset: 0 })
                }}
                style={{
                  background: 'transparent', border: 0, color: 'var(--faint)', padding: '0 0 0 2px',
                  fontSize: 12, lineHeight: 1, cursor: 'pointer',
                }}
              >
                ×
              </button>
            </span>
          ))}
        </div>
      )}
    </div>
  )
}
