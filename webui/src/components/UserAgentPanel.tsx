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
  }, [data])

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
      <div style={{ fontSize: 12, fontWeight: 600, color: '#64748b', letterSpacing: '0.06em', textTransform: 'uppercase', marginBottom: 12 }}>
        Client Identifier
      </div>

      {/* Preset buttons */}
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 12 }}>
        {PRESETS.map(p => (
          <button
            key={p.value}
            onClick={() => setDraft(p.value)}
            style={{
              background: draft === p.value ? '#1e3a5f' : '#1e2433',
              border: '1px solid ' + (draft === p.value ? '#3b82f6' : '#334155'),
              borderRadius: 5,
              color: draft === p.value ? '#93c5fd' : '#94a3b8',
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
          background: '#0f1117',
          border: '1px solid ' + (isCustom && draft ? '#7c3aed' : '#334155'),
          borderRadius: 6,
          color: '#e2e8f0',
          padding: '6px 10px',
          fontSize: 13,
          fontFamily: 'monospace',
          outline: 'none',
          boxSizing: 'border-box',
        }}
      />

      {isCustom && draft && (
        <div style={{ fontSize: 11, color: '#7c3aed', marginTop: 4 }}>Custom value</div>
      )}

      {/* Save button */}
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginTop: 10 }}>
        <button
          onClick={() => mutation.mutate(draft)}
          disabled={!isDirty || mutation.isPending || !draft.trim()}
          style={{
            background: isDirty ? '#2563eb' : '#1e2433',
            border: '1px solid ' + (isDirty ? '#3b82f6' : '#334155'),
            borderRadius: 6,
            color: isDirty ? '#fff' : '#475569',
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

      <div style={{ fontSize: 11, color: '#334155', marginTop: 12, lineHeight: 1.5 }}>
        Current: <span style={{ fontFamily: 'monospace', color: '#475569' }}>{data?.user_agent ?? '…'}</span>
      </div>
    </div>
  )
}
