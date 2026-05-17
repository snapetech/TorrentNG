import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'

interface Props {
  hashes: string[]
  onClose: () => void
}

const INPUT: React.CSSProperties = {
  width: '100%', background: 'var(--bg)', border: '1px solid var(--border-strong)', borderRadius: 5,
  color: 'var(--text)', padding: '6px 8px', fontSize: 12, outline: 'none', boxSizing: 'border-box',
}

export function BulkEditDialog({ hashes, onClose }: Props) {
  const qc = useQueryClient()
  const [category, setCategory] = useState('')
  const [tags, setTags] = useState('')
  const [location, setLocation] = useState('')
  const [ratioLimit, setRatioLimit] = useState('')
  const [seedMinutes, setSeedMinutes] = useState('')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')
  const [messageTone, setMessageTone] = useState<'ok' | 'error'>('ok')

  const { data: categories = [] } = useQuery({ queryKey: ['categories'], queryFn: api.categories.list })
  const { data: allTags = [] } = useQuery({ queryKey: ['tags'], queryFn: api.tags.list })

  async function apply(label: string, fn: () => Promise<void>) {
    setBusy(true)
    setMessage('')
    setMessageTone('ok')
    try {
      await fn()
      setMessage(label)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      qc.invalidateQueries({ queryKey: ['categories'] })
      qc.invalidateQueries({ queryKey: ['tags'] })
      qc.invalidateQueries({ queryKey: ['tracker-health'] })
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err))
      setMessageTone('error')
    } finally {
      setBusy(false)
    }
  }

  const tagList = tags.split(',').map(tag => tag.trim()).filter(Boolean)

  return (
    <div className="rtng-modal-backdrop" style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1200,
      display: 'grid', placeItems: 'center', padding: 22,
    }} onClick={e => { if (!busy && e.target === e.currentTarget) onClose() }}>
      <div role="dialog" aria-modal="true" aria-label="Edit selected torrents" aria-busy={busy} className="rtng-modal" style={{
        width: 'min(620px, 100%)', maxHeight: '88vh', overflowY: 'auto',
        background: 'var(--panel)', border: '1px solid var(--border-strong)', borderRadius: 8,
        boxShadow: '0 24px 60px var(--shadow)',
      }} onClick={e => e.stopPropagation()}>
        <header style={{ padding: '13px 15px', borderBottom: '1px solid var(--border)', display: 'flex', gap: 12, alignItems: 'center' }}>
          <div style={{ flex: 1 }}>
            <div style={{ color: 'var(--text)', fontWeight: 700, fontSize: 15 }}>Edit selected torrents</div>
            <div style={{ color: 'var(--faint)', fontSize: 12 }}>{hashes.length.toLocaleString()} torrent{hashes.length === 1 ? '' : 's'} selected</div>
          </div>
          {busy && <span style={{
            color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
            borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700,
          }}>Applying</span>}
          <button onClick={onClose} disabled={busy} style={smallButton('#94a3b8', busy)}>Close</button>
        </header>

        <div style={{ padding: 15, display: 'grid', gap: 14 }}>
          <Field label="Category">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
              <select value={category} onChange={e => setCategory(e.target.value)} disabled={busy} style={INPUT}>
                <option value="">Clear category</option>
                {categories.map(cat => <option key={cat.name} value={cat.name}>{cat.name}</option>)}
              </select>
              <button disabled={busy} onClick={() => apply('Category applied', async () => { await api.bulk('set-category', hashes, false, { category }) })} style={smallButton('#93c5fd', busy)}>Apply</button>
            </div>
          </Field>

          <Field label="Tags">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
              <input value={tags} onChange={e => setTags(e.target.value)} disabled={busy} placeholder="comma,separated,tags" style={INPUT} />
              <button disabled={busy} onClick={() => apply('Tags applied', () => api.torrents.setTags(hashes, tagList))} style={smallButton('#93c5fd', busy)}>Set tags</button>
            </div>
            {allTags.length > 0 && (
              <div style={{ display: 'flex', gap: 5, flexWrap: 'wrap', marginTop: 7 }}>
                {allTags.map(tag => (
                  <button
                    key={tag}
                    className="rtng-tag-chip"
                    data-active={tagList.includes(tag) ? 'true' : 'false'}
                    disabled={busy}
                    onClick={() => setTags(Array.from(new Set([...tagList, tag])).join(','))}
                    style={{ ...chipButton, opacity: busy ? 0.55 : 1, cursor: busy ? 'not-allowed' : 'pointer' }}
                  >{tag}</button>
                ))}
              </div>
            )}
          </Field>

          <Field label="Location">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 8 }}>
              <input value={location} onChange={e => setLocation(e.target.value)} disabled={busy} placeholder="/downloads/category" style={{ ...INPUT, fontFamily: 'monospace' }} />
              <button disabled={busy || !location.trim()} onClick={() => apply('Location previewed', async () => { await api.bulk('set-location', hashes, true, { save_path: location.trim() }) })} style={smallButton('#94a3b8', busy || !location.trim())}>Preview</button>
              <button disabled={busy || !location.trim()} onClick={() => apply('Location applied', () => api.torrents.setLocation(hashes, location.trim()))} style={smallButton('#93c5fd', busy || !location.trim())}>Move</button>
            </div>
          </Field>

          <Field label="Share limits">
            <div className="rtng-action-grid-3" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr auto', gap: 8 }}>
              <input value={ratioLimit} onChange={e => setRatioLimit(e.target.value)} disabled={busy} placeholder="Ratio -2 default, -1 unlimited" style={INPUT} />
              <input value={seedMinutes} onChange={e => setSeedMinutes(e.target.value)} disabled={busy} placeholder="Minutes -2 default, -1 unlimited" style={INPUT} />
              <button disabled={busy} onClick={() => apply('Share limits applied', () => api.torrents.setShareLimits(
                hashes,
                ratioLimit.trim() ? Number(ratioLimit) : -2,
                seedMinutes.trim() ? Number(seedMinutes) : -2,
              ))} style={smallButton('#93c5fd', busy)}>Apply</button>
            </div>
          </Field>

          <div className="rtng-form-card" style={{
            display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10,
            background: 'var(--surface)', border: '1px solid var(--border)', borderRadius: 7, padding: 10,
          }}>
            <div>
              <div style={{ color: 'var(--text)', fontSize: 12, fontWeight: 700 }}>Playback order</div>
              <div style={{ color: 'var(--faint)', fontSize: 11, marginTop: 2 }}>Toggle sequential mode for every selected torrent.</div>
            </div>
            <button disabled={busy} onClick={() => apply('Sequential toggled', () => api.torrents.toggleSequential(hashes))} style={smallButton('#f59e0b', busy)}>Toggle sequential</button>
          </div>

          {message && <div role={messageTone === 'error' ? 'alert' : 'status'} style={{
            color: messageTone === 'error' ? 'var(--danger)' : 'var(--success)', fontSize: 12,
            background: messageTone === 'error' ? 'color-mix(in srgb, var(--danger) 9%, var(--surface))' : 'color-mix(in srgb, var(--success) 8%, var(--surface))',
            border: '1px solid ' + (messageTone === 'error' ? 'color-mix(in srgb, var(--danger) 45%, var(--border))' : 'color-mix(in srgb, var(--success) 40%, var(--border))'),
            borderRadius: 6, padding: '8px 9px', overflowWrap: 'anywhere',
          }}>{message}</div>}
        </div>
      </div>
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label className="rtng-form-card" style={{
    display: 'grid', gap: 6, color: 'var(--faint)', fontSize: 11, fontWeight: 700,
    textTransform: 'uppercase', background: 'var(--surface)', border: '1px solid var(--border)',
    borderRadius: 7, padding: 10,
    boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.025)',
  }}>
    <span>{label}</span>
    {children}
  </label>
}

function smallButton(color: string, disabled = false): React.CSSProperties {
  return {
    background: 'var(--surface-2)', border: `1px solid ${color}66`, borderRadius: 5,
    color, padding: '6px 9px', fontSize: 12,
    cursor: disabled ? 'not-allowed' : 'pointer', opacity: disabled ? 0.55 : 1,
  }
}

const chipButton: React.CSSProperties = {
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 12,
  color: 'var(--muted)',
  padding: '2px 7px',
  fontSize: 11,
  cursor: 'pointer',
}
