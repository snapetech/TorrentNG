import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type ListParams, type SavedView } from '../api/client'

interface Props {
  params: Omit<ListParams, 'limit' | 'offset'>
  onApply: (params: Omit<ListParams, 'limit' | 'offset'>) => void
}

function cleanParams(params: Omit<ListParams, 'limit' | 'offset'>): Omit<ListParams, 'limit' | 'offset'> {
  return Object.fromEntries(
    Object.entries(params).filter(([, value]) => value !== undefined && value !== ''),
  ) as Omit<ListParams, 'limit' | 'offset'>
}

export function SavedViewsBar({ params, onApply }: Props) {
  const qc = useQueryClient()
  const [name, setName] = useState('')
  const { data: views = [] } = useQuery({
    queryKey: ['saved-views'],
    queryFn: api.savedViews.list,
    staleTime: 30_000,
  })

  async function saveView() {
    const trimmed = name.trim()
    if (!trimmed) return
    const next: SavedView = {
      id: '',
      name: trimmed,
      params: cleanParams(params),
    }
    await api.savedViews.save(next)
    setName('')
    qc.invalidateQueries({ queryKey: ['saved-views'] })
  }

  async function removeView(id: string) {
    await api.savedViews.delete(id)
    qc.invalidateQueries({ queryKey: ['saved-views'] })
  }

  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap',
      padding: '6px 12px', background: '#111827', borderBottom: '1px solid #1e2433',
      fontSize: 12,
    }}>
      <span style={{ color: '#64748b', fontWeight: 600 }}>Views</span>

      {views.map(view => (
        <span key={view.id} style={{
          display: 'inline-flex', alignItems: 'center', gap: 4,
          background: '#1e2433', border: '1px solid #334155', borderRadius: 5,
          overflow: 'hidden',
        }}>
          <button
            onClick={() => onApply(view.params)}
            title={JSON.stringify(view.params)}
            style={{
              background: 'transparent', border: 'none', color: '#94a3b8',
              padding: '3px 8px', fontSize: 12, cursor: 'pointer',
            }}
          >
            {view.name}
          </button>
          <button
            onClick={() => removeView(view.id)}
            style={{
              background: 'transparent', border: 'none', borderLeft: '1px solid #334155',
              color: '#475569', padding: '3px 6px', fontSize: 11, cursor: 'pointer',
            }}
          >
            x
          </button>
        </span>
      ))}

      <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginLeft: views.length ? 4 : 0 }}>
        <input
          value={name}
          onChange={e => setName(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') saveView() }}
          placeholder="Save current view"
          style={{
            width: 150, background: '#0f1117', border: '1px solid #334155',
            borderRadius: 5, color: '#cbd5e1', padding: '3px 8px', fontSize: 12,
            outline: 'none',
          }}
        />
        <button
          onClick={saveView}
          disabled={!name.trim()}
          style={{
            background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
            color: '#93c5fd', padding: '3px 8px', fontSize: 12,
            cursor: name.trim() ? 'pointer' : 'not-allowed',
            opacity: name.trim() ? 1 : 0.5,
          }}
        >
          Save
        </button>
      </div>
    </div>
  )
}
