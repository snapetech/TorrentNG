import { useEffect, useState } from 'react'
import type { ListParams } from '../api/client'

interface Props {
  params: ListParams
  onChange: (p: Partial<ListParams>) => void
}

export function FilterBar({ params, onChange }: Props) {
  const [search, setSearch] = useState(params.filter ?? '')

  useEffect(() => {
    const t = setTimeout(() => onChange({ filter: search, offset: 0 }), 200)
    return () => clearTimeout(t)
  }, [search, onChange])

  return (
    <div style={{
      display: 'flex',
      gap: 8,
      padding: '7px 12px',
      background: '#1a2030',
      borderBottom: '1px solid #2d3748',
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
          background: '#0f1117',
          border: '1px solid #334155',
          borderRadius: 6,
          color: '#e2e8f0',
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
            border: '1px solid #475569',
            borderRadius: 5,
            color: '#64748b',
            padding: '3px 8px',
            fontSize: 11,
            cursor: 'pointer',
          }}
        >
          Clear search
        </button>
      )}
    </div>
  )
}
