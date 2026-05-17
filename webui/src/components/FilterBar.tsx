import { useEffect, useState } from 'react'
import type { ListParams } from '../api/client'

interface Props {
  params: ListParams
  onChange: (p: Partial<ListParams>) => void
}

export function FilterBar({ params, onChange }: Props) {
  const [search, setSearch] = useState(params.filter ?? '')
  const hasSidebarFilters = Boolean(params.status || params.category || params.tag || params.tracker || params.media_type)

  useEffect(() => {
    const t = setTimeout(() => onChange({ filter: search, offset: 0 }), 200)
    return () => clearTimeout(t)
  }, [search, onChange])

  return (
    <div style={{
      display: 'flex',
      gap: 8,
      padding: '7px 12px',
      background: 'var(--surface-2)',
      borderBottom: '1px solid var(--border-strong)',
      alignItems: 'center',
      flexWrap: 'wrap',
    }}>
      <input
        type="search"
        placeholder="Search torrents"
        value={search}
        onChange={e => setSearch(e.target.value)}
        style={{
          flex: '1 1 160px',
          background: 'var(--bg)',
          border: '1px solid var(--border-strong)',
          borderRadius: 6,
          color: 'var(--text)',
          padding: '4px 10px',
          fontSize: 13,
          outline: 'none',
        }}
      />

      {(params.filter || search) && (
        <button
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
        <span style={{ color: 'var(--faint)', fontSize: 11, whiteSpace: 'nowrap' }}>
          Filtered view
        </span>
      )}
    </div>
  )
}
