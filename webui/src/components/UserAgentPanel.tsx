import { useEffect, useRef, useState } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'

const PRESETS = [
  { label: 'rtorrentNG (default)', value: 'rtorrentNG/0.1.0 libtorrent/0.16.11' },
  { label: 'rTorrent 0.16.11',     value: 'rtorrent/0.16.11' },
  { label: 'libtorrent 0.16.11',   value: 'libtorrent/0.16.11' },
  { label: 'qBittorrent 5.0.0',    value: 'qBittorrent/5.0.0' },
  { label: 'Deluge 2.2.0',         value: 'Deluge/2.2.0 libtorrent/2.0.10' },
]

export function UserAgentPanel() {
  const qc = useQueryClient()
  const { data, isLoading } = useQuery({ queryKey: ['user-agent'], queryFn: api.settings.getUserAgent })
  const [draft, setDraft] = useState('')
  const [saved, setSaved] = useState(false)
  const inputRef = useRef<HTMLInputElement>(null)

  useEffect(() => {
    if (data && draft === '') setDraft(data.user_agent)
  }, [data, draft])

  const mutation = useMutation({
    mutationFn: (ua: string) => api.settings.setUserAgent(ua),
    onSuccess: () => {
      qc.invalidateQueries({ queryKey: ['user-agent'] })
      setSaved(true)
      setTimeout(() => setSaved(false), 2000)
    },
  })

  const isDirty = data && draft !== data.user_agent
  const isCustom = !PRESETS.some(p => p.value === draft)

  return (
    <div style={{ padding: '16px 20px', maxWidth: 560 }}>
      <div style={{ fontSize: 12, fontWeight: 600, color: 'var(--faint)', letterSpacing: '0.06em', textTransform: 'uppercase', marginBottom: 12 }}>
        Client Identifier
      </div>

      {/* Preset buttons */}
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 12 }}>
        {PRESETS.map(p => (
          <button
            key={p.value}
            onClick={() => setDraft(p.value)}
            style={{
              background: draft === p.value ? 'var(--accent-soft)' : 'var(--surface-2)',
              border: '1px solid ' + (draft === p.value ? 'var(--accent)' : 'var(--border-strong)'),
              borderRadius: 5,
              color: draft === p.value ? 'var(--accent-text)' : 'var(--muted)',
              padding: '3px 10px',
              fontSize: 11,
              cursor: 'pointer',
              whiteSpace: 'nowrap',
            }}
          >
            {p.label}
          </button>
        ))}
      </div>

      {/* Free-form input */}
      <input
        ref={inputRef}
        type="text"
        value={draft}
        onChange={e => setDraft(e.target.value)}
        placeholder={isLoading ? 'Loading…' : 'user-agent string'}
        style={{
          width: '100%',
          background: 'var(--bg)',
          border: '1px solid ' + (isCustom && draft ? 'var(--accent)' : 'var(--border-strong)'),
          borderRadius: 6,
          color: 'var(--text)',
          padding: '6px 10px',
          fontSize: 13,
          fontFamily: 'monospace',
          outline: 'none',
          boxSizing: 'border-box',
        }}
      />

      {isCustom && draft && (
        <div style={{ fontSize: 11, color: 'var(--accent)', marginTop: 4 }}>Custom value</div>
      )}

      {/* Save button */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginTop: 10 }}>
        <button
          onClick={() => mutation.mutate(draft)}
          disabled={!isDirty || mutation.isPending || !draft.trim()}
          style={{
            background: isDirty ? 'var(--accent)' : 'var(--surface-2)',
            border: '1px solid ' + (isDirty ? 'var(--accent)' : 'var(--border-strong)'),
            borderRadius: 6,
            color: isDirty ? 'var(--accent-text)' : 'var(--faint)',
            padding: '5px 16px',
            fontSize: 12,
            cursor: isDirty ? 'pointer' : 'default',
            fontWeight: 600,
          }}
        >
          {mutation.isPending ? 'Applying…' : 'Apply'}
        </button>
        {saved && <span style={{ fontSize: 12, color: '#22c55e' }}>Applied ✓</span>}
        {mutation.isError && <span style={{ fontSize: 12, color: '#ef4444' }}>Failed</span>}
      </div>

      <div style={{ fontSize: 11, color: 'var(--faint)', marginTop: 12, lineHeight: 1.5 }}>
        Current: <span style={{ fontFamily: 'monospace', color: 'var(--muted)' }}>{data?.user_agent ?? '…'}</span>
      </div>
    </div>
  )
}
