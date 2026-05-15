import { useEffect, useState } from 'react'
import { useQuery } from '@tanstack/react-query'
import type { ListParams } from '../api/client'

const STATUS_OPTIONS = [
  { value: '', label: 'All' },
  { value: 'seeding', label: 'Seeding' },
  { value: 'downloading', label: 'Downloading' },
  { value: 'stopped', label: 'Stopped' },
  { value: 'checking', label: 'Checking' },
]

interface Category { name: string; save_path: string }

async function fetchCategories(): Promise<Category[]> {
  const res = await fetch('/api/v1/categories')
  if (!res.ok) return []
  return res.json()
}

async function fetchTags(): Promise<string[]> {
  const res = await fetch('/api/v1/tags')
  if (!res.ok) return []
  return res.json()
}

interface Props {
  params: ListParams
  onChange: (p: Partial<ListParams>) => void
}

const SELECT_STYLE: React.CSSProperties = {
  background: '#0f1117',
  border: '1px solid #334155',
  borderRadius: 6,
  color: '#94a3b8',
  padding: '4px 8px',
  fontSize: 12,
  outline: 'none',
  cursor: 'pointer',
}

export function FilterBar({ params, onChange }: Props) {
  const [search, setSearch] = useState(params.filter ?? '')

  const { data: categories } = useQuery({
    queryKey: ['categories'],
    queryFn: fetchCategories,
    staleTime: 30_000,
  })

  const { data: tags } = useQuery({
    queryKey: ['tags'],
    queryFn: fetchTags,
    staleTime: 30_000,
  })

  useEffect(() => {
    const t = setTimeout(() => onChange({ filter: search, offset: 0 }), 200)
    return () => clearTimeout(t)
  }, [search])

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
        placeholder="Filter torrents…"
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

      {/* Status filters */}
      <div style={{ display: 'flex', gap: 4, flexWrap: 'wrap' }}>
        {STATUS_OPTIONS.map(o => (
          <button
            key={o.value}
            onClick={() => onChange({ status: o.value || undefined, offset: 0 })}
            style={{
              background: (params.status ?? '') === o.value ? '#2563eb' : '#1e2433',
              border: '1px solid ' + ((params.status ?? '') === o.value ? '#3b82f6' : '#334155'),
              borderRadius: 5,
              color: (params.status ?? '') === o.value ? '#fff' : '#94a3b8',
              padding: '3px 10px',
              fontSize: 12,
              cursor: 'pointer',
            }}
          >
            {o.label}
          </button>
        ))}
      </div>

      {/* Category filter */}
      {categories && categories.length > 0 && (
        <select
          value={params.category ?? ''}
          onChange={e => onChange({ category: e.target.value || undefined, offset: 0 })}
          style={SELECT_STYLE}
        >
          <option value="">All categories</option>
          {categories.map(c => (
            <option key={c.name} value={c.name}>{c.name}</option>
          ))}
        </select>
      )}

      {/* Tag filter */}
      {tags && tags.length > 0 && (
        <select
          value={params.tag ?? ''}
          onChange={e => onChange({ tag: e.target.value || undefined, offset: 0 })}
          style={SELECT_STYLE}
        >
          <option value="">All tags</option>
          {tags.map(t => (
            <option key={t} value={t}>{t}</option>
          ))}
        </select>
      )}

      {/* Clear active filters */}
      {(params.category || params.tag || params.status) && (
        <button
          onClick={() => onChange({ category: undefined, tag: undefined, status: undefined, filter: undefined, offset: 0 })}
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
          ✕ Clear
        </button>
      )}
    </div>
  )
}
