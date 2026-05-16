import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'

interface Props {
  hashes: string[]
  onClear: () => void
}

const ACTIONS = [
  { key: 'start',      label: 'Start',      color: '#22c55e' },
  { key: 'stop',       label: 'Stop',       color: '#64748b' },
  { key: 'recheck',    label: 'Recheck',    color: '#f59e0b' },
  { key: 'reannounce', label: 'Reannounce', color: '#3b82f6' },
]

export function BulkActionBar({ hashes, onClear }: Props) {
  const qc = useQueryClient()
  const [pending, setPending] = useState<string | null>(null)
  const [preview, setPreview] = useState<{ applied: string[] } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [category, setCategory] = useState('')
  const [savePath, setSavePath] = useState('')

  const { data: categories = [] } = useQuery({
    queryKey: ['categories'],
    queryFn: api.categories.list,
    staleTime: 30_000,
  })

  async function runAction(
    action: 'start' | 'stop' | 'recheck' | 'reannounce' | 'set-category' | 'set-location',
    dryRun: boolean,
  ) {
    setPending(action)
    setError(null)
    setPreview(null)
    try {
      const options =
        action === 'set-category' ? { category } :
        action === 'set-location' ? { save_path: savePath.trim() } :
        {}
      const data = await api.bulk(action, hashes, dryRun, options)
      if (dryRun) {
        setPreview(data)
      } else {
        setPreview(null)
        if (data.errors.length > 0) setError(`${data.errors.length} error(s)`)
        qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      }
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  return (
    <div style={{
      minHeight: 44,
      background: 'var(--panel)',
      borderBottom: '1px solid var(--border)',
      display: 'flex',
      alignItems: 'center',
      padding: '6px 12px',
      gap: 8,
      flexWrap: 'wrap',
      flexShrink: 0,
      fontSize: 12,
    }}>
      <span style={{ color: 'var(--accent)', fontWeight: 600, marginRight: 4 }}>
        {hashes.length} selected
      </span>

      {ACTIONS.map(a => (
        <button
          key={a.key}
          disabled={!!pending}
          onClick={() => runAction(a.key as 'start' | 'stop' | 'recheck' | 'reannounce', false)}
          style={{
            background: 'var(--surface-2)',
            border: `1px solid ${a.color}40`,
            borderRadius: 4,
            color: a.color,
            padding: '3px 10px',
            fontSize: 12,
            cursor: pending ? 'not-allowed' : 'pointer',
            opacity: pending ? 0.5 : 1,
          }}
        >
          {pending === a.key ? '…' : a.label}
        </button>
      ))}

      <select
        value={category}
        onChange={e => setCategory(e.target.value)}
        disabled={!!pending}
        style={{
          background: 'var(--surface)', border: '1px solid var(--border-strong)', borderRadius: 4,
          color: 'var(--muted)', padding: '3px 8px', fontSize: 12,
          maxWidth: 150,
        }}
      >
        <option value="">Clear category</option>
        {categories.map(cat => (
          <option key={cat.name} value={cat.name}>{cat.name}</option>
        ))}
      </select>
      <button
        disabled={!!pending}
        onClick={() => runAction('set-category', true)}
        style={{
          background: 'transparent', border: '1px solid var(--border-strong)', borderRadius: 4,
          color: 'var(--muted)', padding: '3px 8px', fontSize: 12,
          cursor: pending ? 'not-allowed' : 'pointer',
        }}
      >
        Preview category
      </button>
      <button
        disabled={!!pending}
        onClick={() => runAction('set-category', false)}
        style={{
          background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 4,
          color: 'var(--accent-text)', padding: '3px 8px', fontSize: 12,
          cursor: pending ? 'not-allowed' : 'pointer',
        }}
      >
        Apply category
      </button>

      <input
        value={savePath}
        onChange={e => setSavePath(e.target.value)}
        disabled={!!pending}
        placeholder="Save path…"
        style={{
          width: 180, background: 'var(--surface)', border: '1px solid var(--border-strong)',
          borderRadius: 4, color: 'var(--text)', padding: '3px 8px',
          fontSize: 12, fontFamily: 'monospace',
        }}
      />
      <button
        disabled={!!pending || !savePath.trim()}
        onClick={() => runAction('set-location', true)}
        style={{
          background: 'transparent', border: '1px solid var(--border-strong)', borderRadius: 4,
          color: 'var(--muted)', padding: '3px 8px', fontSize: 12,
          cursor: pending || !savePath.trim() ? 'not-allowed' : 'pointer',
          opacity: savePath.trim() ? 1 : 0.5,
        }}
      >
        Preview path
      </button>
      <button
        disabled={!!pending || !savePath.trim()}
        onClick={() => runAction('set-location', false)}
        style={{
          background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 4,
          color: 'var(--accent-text)', padding: '3px 8px', fontSize: 12,
          cursor: pending || !savePath.trim() ? 'not-allowed' : 'pointer',
          opacity: savePath.trim() ? 1 : 0.5,
        }}
      >
        Apply path
      </button>

      <button
        disabled={!!pending}
        onClick={() => runAction('stop', true)}
        style={{
          background: 'transparent',
          border: '1px solid var(--border-strong)',
          borderRadius: 4,
          color: 'var(--muted)',
          padding: '3px 10px',
          fontSize: 12,
          cursor: pending ? 'not-allowed' : 'pointer',
          marginLeft: 4,
        }}
      >
        Dry run
      </button>

      {preview && (
        <span style={{ color: 'var(--muted)', fontSize: 11 }}>
          Preview: {preview.applied.length} would be affected
        </span>
      )}
      {error && (
        <span style={{ color: '#ef4444', fontSize: 11 }}>{error}</span>
      )}

      <button
        onClick={onClear}
        style={{
          marginLeft: 'auto',
          background: 'none',
          border: 'none',
          color: 'var(--faint)',
          cursor: 'pointer',
          fontSize: 13,
          padding: '2px 6px',
        }}
      >
        ✕ Clear
      </button>
    </div>
  )
}
