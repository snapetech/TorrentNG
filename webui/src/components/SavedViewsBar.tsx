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
  const activeParams = cleanParams(params)
  const activeKey = JSON.stringify(activeParams)
  const hasActiveFilters = Object.keys(activeParams).length > 0
  const hasSavedCurrentView = views.some(view => JSON.stringify(cleanParams(view.params)) === activeKey)

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
    <div className="tng-savedviews" style={{
      display: 'flex', alignItems: 'center', gap: 8, flexWrap: 'wrap',
      padding: '6px 12px', background: 'var(--surface)', borderBottom: '1px solid var(--border)',
      fontSize: 12,
    }}>
      <span style={{
        color: 'var(--faint)', fontWeight: 800, textTransform: 'uppercase', fontSize: 10,
        letterSpacing: 0, display: 'inline-flex', alignItems: 'center', gap: 5,
      }}>
        <span style={{ color: 'var(--accent)' }}>◇</span>
        Views
      </span>
      {error && <span style={{
        color: 'var(--danger)',
        background: 'color-mix(in srgb, var(--danger) 9%, transparent)',
        border: '1px solid color-mix(in srgb, var(--danger) 38%, var(--border))',
        borderRadius: 999,
        padding: '2px 8px',
      }}>{error}</span>}

      {views.map(view => {
        const isActive = JSON.stringify(cleanParams(view.params)) === activeKey
        return (
          <span key={view.id} className="tng-savedview-chip" data-active={isActive} style={{
            display: 'inline-flex', alignItems: 'center', gap: 4,
            background: isActive ? 'var(--accent-soft)' : 'var(--surface-2)',
            border: '1px solid ' + (isActive ? 'var(--accent)' : 'var(--border-strong)'),
            borderRadius: 5,
            overflow: 'hidden',
          }}>
            <button
              onClick={() => onApply(view.params)}
              disabled={Boolean(busy)}
              title={JSON.stringify(view.params)}
              style={{
                background: 'transparent', border: 'none',
                color: isActive ? 'var(--accent-text)' : 'var(--muted)',
                padding: '3px 8px', fontSize: 12,
                fontWeight: isActive ? 800 : 500,
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
        )
      })}

      {views.length === 0 && !error && (
        <span style={{ color: 'var(--faint)', fontSize: 11 }}>No saved views yet</span>
      )}
      {hasActiveFilters && !hasSavedCurrentView && !error && (
        <span className="tng-unsaved-view" style={{
          color: 'var(--warning)',
          background: 'color-mix(in srgb, var(--warning) 9%, transparent)',
          border: '1px solid color-mix(in srgb, var(--warning) 38%, var(--border))',
          borderRadius: 999,
          padding: '2px 8px',
          fontSize: 11,
          fontWeight: 800,
          whiteSpace: 'nowrap',
        }}>
          Unsaved view
        </span>
      )}

      <div className="tng-savedview-save" style={{ display: 'flex', alignItems: 'center', gap: 6, marginLeft: views.length ? 4 : 0 }}>
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
