import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'

interface Props {
  hashes: string[]
  onClose: () => void
}

const INPUT: React.CSSProperties = {
  width: '100%', background: '#0d1117', border: '1px solid #334155', borderRadius: 5,
  color: '#e2e8f0', padding: '6px 8px', fontSize: 12, outline: 'none', boxSizing: 'border-box',
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

  const { data: categories = [] } = useQuery({ queryKey: ['categories'], queryFn: api.categories.list })
  const { data: allTags = [] } = useQuery({ queryKey: ['tags'], queryFn: api.tags.list })

  async function apply(label: string, fn: () => Promise<void>) {
    setBusy(true)
    setMessage('')
    try {
      await fn()
      setMessage(label)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      qc.invalidateQueries({ queryKey: ['categories'] })
      qc.invalidateQueries({ queryKey: ['tags'] })
      qc.invalidateQueries({ queryKey: ['tracker-health'] })
    } catch (err) {
      setMessage(String(err))
    } finally {
      setBusy(false)
    }
  }

  const tagList = tags.split(',').map(tag => tag.trim()).filter(Boolean)

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1200,
      display: 'grid', placeItems: 'center', padding: 22,
    }}>
      <div style={{
        width: 'min(620px, 100%)', maxHeight: '88vh', overflowY: 'auto',
        background: '#0f141d', border: '1px solid #334155', borderRadius: 8,
        boxShadow: '0 24px 60px rgba(0,0,0,0.5)',
      }}>
        <header style={{ padding: '13px 15px', borderBottom: '1px solid #1e2433', display: 'flex', gap: 12, alignItems: 'center' }}>
          <div style={{ flex: 1 }}>
            <div style={{ color: '#e2e8f0', fontWeight: 700, fontSize: 15 }}>Edit selected torrents</div>
            <div style={{ color: '#64748b', fontSize: 12 }}>{hashes.length.toLocaleString()} torrent{hashes.length === 1 ? '' : 's'} selected</div>
          </div>
          <button onClick={onClose} style={smallButton('#94a3b8')}>Close</button>
        </header>

        <div style={{ padding: 15, display: 'grid', gap: 14 }}>
          <Field label="Category">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
              <select value={category} onChange={e => setCategory(e.target.value)} style={INPUT}>
                <option value="">Clear category</option>
                {categories.map(cat => <option key={cat.name} value={cat.name}>{cat.name}</option>)}
              </select>
              <button disabled={busy} onClick={() => apply('Category applied', async () => { await api.bulk('set-category', hashes, false, { category }) })} style={smallButton('#93c5fd')}>Apply</button>
            </div>
          </Field>

          <Field label="Tags">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
              <input value={tags} onChange={e => setTags(e.target.value)} placeholder="comma,separated,tags" style={INPUT} />
              <button disabled={busy} onClick={() => apply('Tags applied', () => api.torrents.setTags(hashes, tagList))} style={smallButton('#93c5fd')}>Set tags</button>
            </div>
            {allTags.length > 0 && (
              <div style={{ display: 'flex', gap: 5, flexWrap: 'wrap', marginTop: 7 }}>
                {allTags.map(tag => (
                  <button key={tag} onClick={() => setTags(Array.from(new Set([...tagList, tag])).join(','))} style={chipButton}>{tag}</button>
                ))}
              </div>
            )}
          </Field>

          <Field label="Location">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 8 }}>
              <input value={location} onChange={e => setLocation(e.target.value)} placeholder="/downloads/category" style={{ ...INPUT, fontFamily: 'monospace' }} />
              <button disabled={busy || !location.trim()} onClick={() => apply('Location previewed', async () => { await api.bulk('set-location', hashes, true, { save_path: location.trim() }) })} style={smallButton('#94a3b8')}>Preview</button>
              <button disabled={busy || !location.trim()} onClick={() => apply('Location applied', () => api.torrents.setLocation(hashes, location.trim()))} style={smallButton('#93c5fd')}>Move</button>
            </div>
          </Field>

          <Field label="Share limits">
            <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr auto', gap: 8 }}>
              <input value={ratioLimit} onChange={e => setRatioLimit(e.target.value)} placeholder="Ratio -2 default, -1 unlimited" style={INPUT} />
              <input value={seedMinutes} onChange={e => setSeedMinutes(e.target.value)} placeholder="Minutes -2 default, -1 unlimited" style={INPUT} />
              <button disabled={busy} onClick={() => apply('Share limits applied', () => api.torrents.setShareLimits(
                hashes,
                ratioLimit.trim() ? Number(ratioLimit) : -2,
                seedMinutes.trim() ? Number(seedMinutes) : -2,
              ))} style={smallButton('#93c5fd')}>Apply</button>
            </div>
          </Field>

          <div style={{ display: 'flex', gap: 8 }}>
            <button disabled={busy} onClick={() => apply('Sequential toggled', () => api.torrents.toggleSequential(hashes))} style={smallButton('#f59e0b')}>Toggle sequential download</button>
          </div>

          {message && <div style={{ color: message.startsWith('Error') ? '#f87171' : '#94a3b8', fontSize: 12 }}>{message}</div>}
        </div>
      </div>
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label style={{ display: 'grid', gap: 5, color: '#64748b', fontSize: 11, fontWeight: 700, textTransform: 'uppercase' }}>{label}{children}</label>
}

function smallButton(color: string): React.CSSProperties {
  return {
    background: '#1e2433', border: `1px solid ${color}66`, borderRadius: 5,
    color, padding: '6px 9px', fontSize: 12, cursor: 'pointer',
  }
}

const chipButton: React.CSSProperties = {
  background: '#111827',
  border: '1px solid #334155',
  borderRadius: 12,
  color: '#94a3b8',
  padding: '2px 7px',
  fontSize: 11,
  cursor: 'pointer',
}
