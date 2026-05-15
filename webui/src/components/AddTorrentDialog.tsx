import { useState, useRef, useCallback } from 'react'
import { useQueryClient } from '@tanstack/react-query'
import { useQuery } from '@tanstack/react-query'

interface Category { name: string; save_path: string }

async function fetchCategories(): Promise<Category[]> {
  const res = await fetch('/api/v1/categories')
  if (!res.ok) return []
  return res.json()
}

interface Props {
  onClose: () => void
}

const INPUT: React.CSSProperties = {
  background: '#0f1117', border: '1px solid #334155', borderRadius: 5,
  color: '#e2e8f0', padding: '6px 10px', fontSize: 13, outline: 'none',
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

  const { data: categories = [] } = useQuery({ queryKey: ['categories'], queryFn: fetchCategories })

  const addFiles = useCallback((incoming: FileList | File[]) => {
    const valid = Array.from(incoming).filter(f => f.name.endsWith('.torrent'))
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
        const fd = new FormData()
        fd.append('torrent', file)
        fd.append('save_path', effectiveSavePath)
        fd.append('start', String(start))
        if (category) fd.append('category', category)
        const res = await fetch('/api/v1/torrents', { method: 'POST', body: fd })
        if (!res.ok) throw new Error(`Failed to add ${file.name}`)
      }

      // Load magnet/HTTP URLs
      for (const line of url.split('\n')) {
        const u = line.trim()
        if (!u) continue
        const fd = new FormData()
        fd.append('magnet', u)
        fd.append('save_path', effectiveSavePath)
        fd.append('start', String(start))
        const res = await fetch('/api/v1/torrents', { method: 'POST', body: fd })
        if (!res.ok) throw new Error(`Failed to add ${u}`)
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
      position: 'fixed', inset: 0, background: '#000a', display: 'flex',
      alignItems: 'center', justifyContent: 'center', zIndex: 100,
    }} onClick={e => { if (e.target === e.currentTarget) onClose() }}>
      <div style={{
        background: '#111827', border: '1px solid #1e2433', borderRadius: 10,
        width: 480, maxWidth: '95vw', padding: 24, display: 'flex', flexDirection: 'column', gap: 16,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between' }}>
          <span style={{ fontWeight: 600, fontSize: 15, color: '#e2e8f0' }}>Add torrent</span>
          <button onClick={onClose} style={{ background: 'none', border: 'none', color: '#475569', fontSize: 18, cursor: 'pointer' }}>✕</button>
        </div>

        {/* Drag-drop zone */}
        <div
          onDragOver={e => { e.preventDefault(); setDragOver(true) }}
          onDragLeave={() => setDragOver(false)}
          onDrop={handleDrop}
          onClick={() => fileRef.current?.click()}
          style={{
            border: `2px dashed ${dragOver ? '#3b82f6' : '#334155'}`,
            borderRadius: 8, padding: '20px 16px', textAlign: 'center',
            cursor: 'pointer', transition: 'border-color 0.15s',
            background: dragOver ? '#0f1e3a' : 'transparent',
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
            <span style={{ fontSize: 13, color: '#64748b' }}>
              Drop .torrent files here or <span style={{ color: '#3b82f6' }}>browse</span>
            </span>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {files.map(f => (
                <div key={f.name} style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
                  <span style={{ fontSize: 12, color: '#94a3b8', flex: 1, textAlign: 'left' }}>📄 {f.name}</span>
                  <button onClick={e => { e.stopPropagation(); setFiles(p => p.filter(x => x !== f)) }}
                    style={{ background: 'none', border: 'none', color: '#ef4444', cursor: 'pointer', fontSize: 14, padding: '0 2px' }}>✕</button>
                </div>
              ))}
              <span style={{ fontSize: 11, color: '#475569', marginTop: 4 }}>Click to add more</span>
            </div>
          )}
        </div>

        {/* URL input */}
        <div>
          <label style={{ fontSize: 11, color: '#64748b', display: 'block', marginBottom: 4 }}>
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
          <label style={{ fontSize: 11, color: '#64748b', display: 'block', marginBottom: 4 }}>Save path</label>
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
            <label style={{ fontSize: 11, color: '#64748b', display: 'block', marginBottom: 4 }}>Category</label>
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
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: 'pointer', fontSize: 13, color: '#94a3b8' }}>
          <input type="checkbox" checked={start} onChange={e => setStart(e.target.checked)} style={{ accentColor: '#3b82f6' }} />
          Start immediately
        </label>

        {error && <div style={{ fontSize: 12, color: '#ef4444' }}>{error}</div>}

        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end' }}>
          <button onClick={onClose} style={{
            background: 'none', border: '1px solid #334155', borderRadius: 5,
            color: '#64748b', padding: '6px 16px', fontSize: 13, cursor: 'pointer',
          }}>Cancel</button>
          <button onClick={submit} disabled={busy} style={{
            background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
            color: '#93c5fd', padding: '6px 20px', fontSize: 13,
            cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.6 : 1,
          }}>
            {busy ? 'Adding…' : 'Add'}
          </button>
        </div>
      </div>
    </div>
  )
}
