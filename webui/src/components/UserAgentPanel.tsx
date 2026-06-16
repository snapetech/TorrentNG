import { useEffect, useRef, useState } from 'react'
import { useQuery, useMutation, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'

const PRESETS = [
  { label: 'rTorrent 0.16.11',     value: 'rtorrent/0.16.11/0.16.11' },
  { label: 'libtorrent 0.16.11',   value: 'libtorrent/0.16.11' },
  { label: 'qBittorrent 5.0.0',    value: 'qBittorrent/5.0.0' },
  { label: 'Deluge 2.2.0',         value: 'Deluge/2.2.0 libtorrent/2.0.10' },
]

export function UserAgentPanel() {
  const qc = useQueryClient()
  const { data: engine } = useQuery({
    queryKey: ['engine'],
    queryFn: api.engine,
    staleTime: 5_000,
  })
  const supported = engine?.backend.capabilities.supports_runtime_user_agent !== false
  const { data, isLoading } = useQuery({
    queryKey: ['user-agent'],
    queryFn: api.settings.getUserAgent,
    enabled: supported,
  })
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

  if (!supported) {
    return (
      <div style={{ padding: '18px 20px', maxWidth: 680 }}>
        <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)' }}>Client Identifier</div>
        <div style={noticeStyle}>The selected backend does not support runtime tracker user-agent changes.</div>
      </div>
    )
  }

  return (
    <div style={{ padding: '18px 20px', maxWidth: 680 }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
        <div>
          <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)' }}>
            Client Identifier
          </div>
          <div style={{ color: 'var(--faint)', fontSize: 12, marginTop: 3 }}>
            Tracker-facing user agent
          </div>
        </div>
        {isDirty && <span style={{
          color: 'var(--warning)', background: 'color-mix(in srgb, var(--warning) 10%, var(--surface))',
          border: '1px solid color-mix(in srgb, var(--warning) 45%, var(--border))',
          borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700,
        }}>Unsaved</span>}
      </div>

      <div className="tng-card tng-identity-panel" data-dirty={isDirty ? 'true' : 'false'} style={panelStyle}>
        <div style={{ fontSize: 12, fontWeight: 800, color: 'var(--text)', marginBottom: 10 }}>Preset identity</div>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginBottom: 12 }}>
          {PRESETS.map(p => (
            <button
              key={p.value}
              className="tng-preset-button"
              data-active={draft === p.value ? 'true' : 'false'}
              onClick={() => setDraft(p.value)}
              disabled={mutation.isPending}
              style={presetButton(draft === p.value, mutation.isPending)}
            >
              {p.label}
            </button>
          ))}
        </div>

        <label style={{ display: 'grid', gap: 5 }}>
          <span style={labelStyle}>Custom user agent</span>
          <input
            ref={inputRef}
            type="text"
            value={draft}
            onChange={e => setDraft(e.target.value)}
            disabled={mutation.isPending}
            placeholder={isLoading ? 'Loading…' : 'user-agent string'}
            style={{
              width: '100%',
              background: 'var(--bg)',
              border: '1px solid ' + (isCustom && draft ? 'var(--accent)' : 'var(--border-strong)'),
              borderRadius: 6,
              color: 'var(--text)',
              padding: '7px 10px',
              fontSize: 13,
              fontFamily: 'monospace',
              outline: 'none',
              boxSizing: 'border-box',
            }}
          />
        </label>

        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginTop: 10 }}>
          <div style={{ display: 'flex', gap: 6, flexWrap: 'wrap' }}>
            {isCustom && draft && <Pill tone="info">Custom value</Pill>}
            {saved && <Pill tone="ok">Applied</Pill>}
            {mutation.isPending && <Pill tone="info">Applying</Pill>}
          </div>
          <button
            onClick={() => mutation.mutate(draft)}
            disabled={!isDirty || mutation.isPending || !draft.trim()}
            style={applyButton(Boolean(isDirty) && !mutation.isPending && Boolean(draft.trim()))}
          >
            {mutation.isPending ? 'Applying…' : 'Apply'}
          </button>
        </div>

        {mutation.isError && (
          <div style={noticeStyle}>
            {mutation.error instanceof Error ? mutation.error.message : 'Failed'}
          </div>
        )}
      </div>

      <div className="tng-metric-tile" style={currentStyle}>
        <span style={{ color: 'var(--faint)', fontWeight: 800, textTransform: 'uppercase', fontSize: 10 }}>Current</span>
        <span style={{ fontFamily: 'monospace', color: 'var(--muted)', overflowWrap: 'anywhere' }}>{data?.user_agent ?? '…'}</span>
      </div>
    </div>
  )
}

const panelStyle: React.CSSProperties = {
  background: 'color-mix(in srgb, var(--surface) 84%, var(--bg))',
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: 12,
  boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.03)',
}

const labelStyle: React.CSSProperties = {
  color: 'var(--faint)',
  fontSize: 10,
  fontWeight: 800,
  textTransform: 'uppercase',
  letterSpacing: 0,
}

const currentStyle: React.CSSProperties = {
  display: 'grid',
  gap: 5,
  fontSize: 11,
  color: 'var(--faint)',
  marginTop: 12,
  lineHeight: 1.45,
  background: 'var(--surface)',
  border: '1px solid var(--border)',
  borderRadius: 7,
  padding: '9px 10px',
}

const noticeStyle: React.CSSProperties = {
  color: 'var(--danger)',
  background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
  border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
  borderRadius: 6,
  padding: '8px 9px',
  fontSize: 12,
  marginTop: 10,
  overflowWrap: 'anywhere',
}

function presetButton(active: boolean, disabled: boolean): React.CSSProperties {
  return {
    background: active ? 'var(--accent-soft)' : 'var(--surface-2)',
    border: '1px solid ' + (active ? 'var(--accent)' : 'var(--border-strong)'),
    borderRadius: 5,
    color: active ? 'var(--accent-text)' : 'var(--muted)',
    padding: '4px 10px',
    fontSize: 11,
    fontWeight: active ? 800 : 600,
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.55 : 1,
    whiteSpace: 'nowrap',
  }
}

function applyButton(enabled: boolean): React.CSSProperties {
  return {
    background: enabled ? 'var(--accent)' : 'var(--surface-2)',
    border: '1px solid ' + (enabled ? 'var(--accent)' : 'var(--border-strong)'),
    borderRadius: 6,
    color: enabled ? 'var(--accent-text)' : 'var(--faint)',
    padding: '6px 16px',
    fontSize: 12,
    cursor: enabled ? 'pointer' : 'default',
    opacity: enabled ? 1 : 0.6,
    fontWeight: 700,
  }
}

function Pill({ tone, children }: { tone: 'ok' | 'info'; children: React.ReactNode }) {
  const color = tone === 'ok' ? 'var(--success)' : 'var(--accent)'
  return <span style={{
    color,
    background: `color-mix(in srgb, ${color} 8%, transparent)`,
    border: `1px solid color-mix(in srgb, ${color} 45%, var(--border))`,
    borderRadius: 999,
    padding: '2px 8px',
    fontSize: 11,
    fontWeight: 800,
  }}>{children}</span>
}
