import { useState, useRef, useCallback } from 'react'
import { useQueryClient, useQuery } from '@tanstack/react-query'
import { api } from '../api/client'

interface Props {
  onClose: () => void
}

const INPUT: React.CSSProperties = {
  background: 'var(--bg)', border: '1px solid var(--border-strong)', borderRadius: 5,
  color: 'var(--text)', padding: '6px 10px', fontSize: 13, outline: 'none',
  width: '100%', boxSizing: 'border-box',
}

export function AddTorrentDialog({ onClose }: Props) {
  const qc = useQueryClient()
  const [url, setUrl] = useState('')
  const [savePath, setSavePath] = useState('')
  const [category, setCategory] = useState('')
  const [start, setStart] = useState(true)
  const [dragOver, setDragOver] = useState(false)
  const [files, setFiles] = useState<File[]>([])
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const fileRef = useRef<HTMLInputElement>(null)

  const { data: categories = [] } = useQuery({ queryKey: ['categories'], queryFn: api.categories.list })

  const addFiles = useCallback((incoming: FileList | File[]) => {
    const all = Array.from(incoming)
    const valid = all.filter(f => f.name.toLowerCase().endsWith('.torrent'))
    if (valid.length !== all.length) {
      setError('Only .torrent files can be added from the file picker.')
    } else {
      setError(null)
    }
    setFiles(prev => {
      const names = new Set(prev.map(f => f.name))
      return [...prev, ...valid.filter(f => !names.has(f.name))]
    })
  }, [])

  function handleDrop(e: React.DragEvent) {
    e.preventDefault()
    setDragOver(false)
    addFiles(e.dataTransfer.files)
  }

  async function submit() {
    if (!url.trim() && files.length === 0) {
      setError('Enter a URL or drag a .torrent file.')
      return
    }
    setBusy(true)
    setError(null)
    try {
      const effectiveSavePath = savePath.trim()

      // Upload .torrent files
      for (const file of files) {
        await api.torrents.addFile(file, effectiveSavePath, category, start)
      }

      // Load magnet/HTTP URLs
      for (const line of url.split('\n')) {
        const u = line.trim()
        if (!u) continue
        await api.torrents.addMagnet(u, effectiveSavePath, category, start)
      }

      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      onClose()
    } catch (e) {
      setError(String(e))
    } finally {
      setBusy(false)
    }
  }

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', display: 'flex',
      alignItems: 'center', justifyContent: 'center', zIndex: 100,
    }} onClick={e => { if (e.target === e.currentTarget) onClose() }}>
      <div style={{
        background: 'var(--panel)', border: '1px solid var(--border)', borderRadius: 10,
        width: 480, maxWidth: '95vw', padding: 24, display: 'flex', flexDirection: 'column', gap: 16,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <span style={{ fontWeight: 600, fontSize: 15, color: 'var(--text)' }}>Add torrent</span>
          <button
            onClick={onClose}
            disabled={busy}
            style={{
              background: 'none', border: 'none', color: 'var(--faint)', fontSize: 18,
              cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.55 : 1,
            }}
          >✕</button>
        </div>

        {/* Drag-drop zone */}
        <div
          onDragOver={e => { e.preventDefault(); setDragOver(true) }}
          onDragLeave={() => setDragOver(false)}
          onDrop={handleDrop}
          onClick={() => fileRef.current?.click()}
          style={{
            border: `2px dashed ${dragOver ? 'var(--accent)' : 'var(--border-strong)'}`,
            borderRadius: 8, padding: '20px 16px', textAlign: 'center',
            cursor: 'pointer', transition: 'border-color 0.15s',
            background: dragOver ? 'var(--accent-soft)' : 'transparent',
          }}
        >
          <input
            ref={fileRef}
            type="file"
            accept=".torrent"
            multiple
            style={{ display: 'none' }}
            onChange={e => e.target.files && addFiles(e.target.files)}
          />
          {files.length === 0 ? (
            <span style={{ fontSize: 13, color: 'var(--faint)' }}>
              Drop .torrent files here or <span style={{ color: 'var(--accent)' }}>browse</span>
            </span>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {files.map(f => (
                <div key={f.name} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span style={{ fontSize: 12, color: 'var(--muted)', flex: 1, textAlign: 'left' }}>📄 {f.name}</span>
                  <button onClick={e => { e.stopPropagation(); setFiles(p => p.filter(x => x !== f)) }}
                    style={{ background: 'none', border: 'none', color: '#ef4444', cursor: 'pointer', fontSize: 14, padding: '0 2px' }}>✕</button>
                </div>
              ))}
              <span style={{ fontSize: 11, color: 'var(--faint)', marginTop: 4 }}>Click to add more</span>
            </div>
          )}
        </div>

        {/* URL input */}
        <div>
          <label style={{ fontSize: 11, color: 'var(--faint)', display: 'block', marginBottom: 4 }}>
            Magnet links or URLs (one per line)
          </label>
          <textarea
            value={url}
            onChange={e => setUrl(e.target.value)}
            placeholder="magnet:?xt=urn:btih:…"
            rows={3}
            style={{ ...INPUT, resize: 'vertical', fontFamily: 'monospace', fontSize: 12 }}
          />
        </div>

        {/* Save path */}
        <div>
          <label style={{ fontSize: 11, color: 'var(--faint)', display: 'block', marginBottom: 4 }}>Save path</label>
          <input
            value={savePath}
            onChange={e => setSavePath(e.target.value)}
            placeholder="/data/downloads"
            style={INPUT}
          />
        </div>

        {/* Category */}
        {categories.length > 0 && (
          <div>
            <label style={{ fontSize: 11, color: 'var(--faint)', display: 'block', marginBottom: 4 }}>Category</label>
            <select
              value={category}
              onChange={e => {
                setCategory(e.target.value)
                const cat = categories.find(c => c.name === e.target.value)
                if (cat?.save_path && !savePath) setSavePath(cat.save_path)
              }}
              style={{ ...INPUT, cursor: 'pointer' }}
            >
              <option value="">None</option>
              {categories.map(c => <option key={c.name} value={c.name}>{c.name}</option>)}
            </select>
          </div>
        )}

        {/* Start toggle */}
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: busy ? 'not-allowed' : 'pointer', fontSize: 13, color: 'var(--muted)' }}>
          <input type="checkbox" checked={start} disabled={busy} onChange={e => setStart(e.target.checked)} style={{ accentColor: 'var(--accent)' }} />
          Start immediately
        </label>

        {error && <div style={{ fontSize: 12, color: '#ef4444' }}>{error}</div>}

        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button onClick={onClose} disabled={busy} style={{
            background: 'none', border: '1px solid var(--border-strong)', borderRadius: 5,
            color: 'var(--faint)', padding: '6px 16px', fontSize: 13, cursor: 'pointer',
            opacity: busy ? 0.5 : 1,
          }}>Cancel</button>
          <button onClick={submit} disabled={busy} style={{
            background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
            color: 'var(--accent-text)', padding: '6px 20px', fontSize: 13,
            cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.6 : 1,
          }}>
            {busy ? 'Adding…' : 'Add'}
          </button>
        </div>
      </div>
    </div>
  )
}
