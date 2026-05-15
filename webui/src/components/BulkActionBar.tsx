import { useState } from 'react'

interface BulkResult {
  applied: string[]
  errors: string[]
  dry_run: boolean
}

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
  const [pending, setPending] = useState<string | null>(null)
  const [preview, setPreview] = useState<BulkResult | null>(null)
  const [error, setError] = useState<string | null>(null)

  async function runAction(action: string, dryRun: boolean) {
    setPending(action)
    setError(null)
    setPreview(null)
    try {
      const res = await fetch(`/api/v1/bulk/${action}`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', 'X-RTNG-CSRF': '1' },
        body: JSON.stringify({ hashes, dry_run: dryRun }),
      })
      if (!res.ok) throw new Error(`HTTP ${res.status}`)
      const data: BulkResult = await res.json()
      if (dryRun) {
        setPreview(data)
      } else {
        setPreview(null)
        if (data.errors.length > 0) setError(`${data.errors.length} error(s)`)
      }
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  return (
    <div style={{
      height: 44,
      background: '#0a1628',
      borderBottom: '1px solid #1e3a5f',
      display: 'flex',
      alignItems: 'center',
      padding: '0 12px',
      gap: 8,
      flexShrink: 0,
      fontSize: 12,
    }}>
      <span style={{ color: '#3b82f6', fontWeight: 600, marginRight: 4 }}>
        {hashes.length} selected
      </span>

      {ACTIONS.map(a => (
        <button
          key={a.key}
          disabled={!!pending}
          onClick={() => runAction(a.key, false)}
          style={{
            background: '#1e2433',
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

      <button
        disabled={!!pending}
        onClick={() => runAction('stop', true)}
        style={{
          background: 'transparent',
          border: '1px solid #475569',
          borderRadius: 4,
          color: '#94a3b8',
          padding: '3px 10px',
          fontSize: 12,
          cursor: pending ? 'not-allowed' : 'pointer',
          marginLeft: 4,
        }}
      >
        Dry run
      </button>

      {preview && (
        <span style={{ color: '#94a3b8', fontSize: 11 }}>
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
          color: '#475569',
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
