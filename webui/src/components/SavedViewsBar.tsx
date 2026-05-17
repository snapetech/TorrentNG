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
  const [busy, setBusy] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const { data: views = [] } = useQuery({
    queryKey: ['saved-views'],
    queryFn: api.savedViews.list,
    staleTime: 30_000,
  })

  async function saveView() {
    const trimmed = name.trim()
    if (!trimmed || busy) return
    const next: SavedView = {
      id: '',
      name: trimmed,
      params: cleanParams(params),
    }
    setBusy('__save__')
    setError(null)
    try {
      await api.savedViews.save(next)
      setName('')
      qc.invalidateQueries({ queryKey: ['saved-views'] })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to save view.')
    } finally {
      setBusy(null)
    }
  }

  async function removeView(id: string) {
    if (busy) return
    setBusy(id)
    setError(null)
    try {
      await api.savedViews.delete(id)
      qc.invalidateQueries({ queryKey: ['saved-views'] })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Failed to delete view.')
    } finally {
      setBusy(null)
    }
  }

  return (
    <div style={{
      display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap',
      padding: '6px 12px', background: 'var(--surface)', borderBottom: '1px solid var(--border)',
      fontSize: 12,
    }}>
      <span style={{ color: 'var(--faint)', fontWeight: 600 }}>Views</span>
      {error && <span style={{ color: 'var(--danger)' }}>{error}</span>}

      {views.map(view => (
        <span key={view.id} style={{
          display: 'inline-flex', alignItems: 'center', gap: 4,
          background: 'var(--surface-2)', border: '1px solid var(--border-strong)', borderRadius: 5,
          overflow: 'hidden',
        }}>
          <button
            onClick={() => onApply(view.params)}
            disabled={Boolean(busy)}
            title={JSON.stringify(view.params)}
            style={{
              background: 'transparent', border: 'none', color: 'var(--muted)',
              padding: '3px 8px', fontSize: 12,
              cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.55 : 1,
            }}
          >
            {view.name}
          </button>
          <button
            onClick={() => removeView(view.id)}
            disabled={Boolean(busy)}
            style={{
              background: 'transparent', border: 'none', borderLeft: '1px solid var(--border-strong)',
              color: 'var(--faint)', padding: '3px 6px', fontSize: 11,
              cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.55 : 1,
            }}
          >
            {busy === view.id ? '…' : 'x'}
          </button>
        </span>
      ))}

      <div style={{ display: 'flex', alignItems: 'center', gap: 6, marginLeft: views.length ? 4 : 0 }}>
        <input
          value={name}
          onChange={e => setName(e.target.value)}
          onKeyDown={e => { if (e.key === 'Enter') saveView() }}
          disabled={Boolean(busy)}
          placeholder="Save current view"
          style={{
            width: 150, background: 'var(--surface)', border: '1px solid var(--border-strong)',
            borderRadius: 5, color: 'var(--text)', padding: '3px 8px', fontSize: 12,
            outline: 'none',
          }}
        />
        <button
          onClick={saveView}
          disabled={!name.trim() || Boolean(busy)}
          style={{
            background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
            color: 'var(--accent-text)', padding: '3px 8px', fontSize: 12,
            cursor: name.trim() && !busy ? 'pointer' : 'not-allowed',
            opacity: name.trim() && !busy ? 1 : 0.5,
          }}
        >
          {busy === '__save__' ? 'Saving…' : 'Save'}
        </button>
      </div>
    </div>
  )
}
