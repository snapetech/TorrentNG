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
  const urlCount = url.split('\n').map(line => line.trim()).filter(Boolean).length
  const canSubmit = !busy && (urlCount > 0 || files.length > 0)
  const selectedCategory = categories.find(c => c.name === category)
  const effectiveSavePath = savePath.trim() || selectedCategory?.save_path || ''

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
    <div className="rtng-modal-backdrop" style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', display: 'flex',
      alignItems: 'center', justifyContent: 'center', zIndex: 100,
    }} onClick={e => { if (!busy && e.target === e.currentTarget) onClose() }}>
      <div role="dialog" aria-modal="true" aria-label="Add torrent" className="rtng-modal rtng-add-dialog" style={{
        background: 'var(--panel)', border: '1px solid var(--border)', borderRadius: 10,
        width: 480, maxWidth: '95vw', padding: 24, display: 'flex', flexDirection: 'column', gap: 16,
      }}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12 }}>
          <div>
            <div style={{ fontWeight: 800, fontSize: 15, color: 'var(--text)' }}>Add torrent</div>
            <div style={{ color: 'var(--faint)', fontSize: 12, marginTop: 2 }}>Stage files, magnets, or HTTP torrent URLs</div>
          </div>
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
          className="rtng-dropzone"
          data-active={dragOver ? 'true' : 'false'}
          data-filled={files.length > 0 ? 'true' : 'false'}
          role="button"
          tabIndex={0}
          aria-label="Choose or drop torrent files"
          onDragOver={e => { e.preventDefault(); setDragOver(true) }}
          onDragLeave={() => setDragOver(false)}
          onDrop={handleDrop}
          onClick={() => fileRef.current?.click()}
          onKeyDown={e => {
            if (e.key === 'Enter' || e.key === ' ') {
              e.preventDefault()
              fileRef.current?.click()
            }
          }}
          style={{
            border: `2px dashed ${dragOver ? 'var(--accent)' : files.length ? 'var(--success)' : 'var(--border-strong)'}`,
            borderRadius: 8, padding: '20px 16px', textAlign: 'center',
            cursor: 'pointer', transition: 'border-color 0.15s',
            background: dragOver ? 'var(--accent-soft)' : files.length ? 'color-mix(in srgb, var(--success) 7%, transparent)' : 'transparent',
            boxShadow: dragOver ? '0 0 0 3px color-mix(in srgb, var(--accent) 16%, transparent)' : undefined,
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
            <div style={{ display: 'grid', gap: 5 }}>
              <span style={{ fontSize: 14, color: dragOver ? 'var(--accent-text)' : 'var(--muted)', fontWeight: 700 }}>
                {dragOver ? 'Drop to stage torrents' : 'Drop .torrent files here'}
              </span>
              <span style={{ fontSize: 12, color: 'var(--faint)' }}>
                or <span style={{ color: 'var(--accent)' }}>browse</span>
              </span>
            </div>
          ) : (
            <div style={{ display: 'flex', flexDirection: 'column', gap: 4 }}>
              {files.map(f => (
                <div key={f.name} className="rtng-staged-file" style={{
                  display: 'flex', alignItems: 'center', gap: 8,
                  border: '1px solid var(--border)', borderRadius: 6, padding: '5px 7px',
                  background: 'var(--surface)',
                }}>
                  <span style={{ fontSize: 12, color: 'var(--muted)', flex: 1, textAlign: 'left', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>📄 {f.name}</span>
                  <span style={{ color: 'var(--faint)', fontSize: 10, fontVariantNumeric: 'tabular-nums' }}>{fmtSize(f.size)}</span>
                  <button aria-label={`Remove ${f.name}`} onClick={e => { e.stopPropagation(); setFiles(p => p.filter(x => x !== f)) }}
                    style={{ background: 'none', border: 'none', color: '#ef4444', cursor: 'pointer', fontSize: 14, padding: '0 2px' }}>✕</button>
                </div>
              ))}
              <span style={{ fontSize: 11, color: 'var(--success)', marginTop: 4 }}>
                {files.length.toLocaleString()} file{files.length === 1 ? '' : 's'} staged · click to add more
              </span>
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
          {urlCount > 0 && (
            <div style={{ marginTop: 5, color: 'var(--accent-text)', fontSize: 11 }}>
              {urlCount.toLocaleString()} URL{urlCount === 1 ? '' : 's'} staged
            </div>
          )}
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
            {selectedCategory?.save_path && (
              <div style={{ marginTop: 5, color: 'var(--faint)', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                Category path: <span style={{ color: 'var(--muted)' }}>{selectedCategory.save_path}</span>
              </div>
            )}
          </div>
        )}

        {/* Start toggle */}
        <label style={{ display: 'flex', alignItems: 'center', gap: 8, cursor: busy ? 'not-allowed' : 'pointer', fontSize: 13, color: 'var(--muted)' }}>
          <input type="checkbox" checked={start} disabled={busy} onChange={e => setStart(e.target.checked)} style={{ accentColor: 'var(--accent)' }} />
          Start immediately
        </label>

        <div style={{
          display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10,
          padding: '7px 9px', border: '1px solid var(--border)', borderRadius: 6,
          background: 'var(--surface)', color: 'var(--faint)', fontSize: 12,
        }}>
          <span style={{ minWidth: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
            {files.length.toLocaleString()} file{files.length === 1 ? '' : 's'} · {urlCount.toLocaleString()} URL{urlCount === 1 ? '' : 's'}
            {effectiveSavePath ? ` · ${effectiveSavePath}` : ''}
          </span>
          <span style={{ color: start ? 'var(--success)' : 'var(--warning)' }}>
            {start ? 'will start' : 'paused on add'}
          </span>
        </div>

        {error && <div style={noticeStyle}>{error}</div>}

        <div style={{ display: 'flex', gap: 8, justifyContent: 'flex-end', flexWrap: 'wrap' }}>
          <button onClick={onClose} disabled={busy} style={{
            background: 'none', border: '1px solid var(--border-strong)', borderRadius: 5,
            color: 'var(--faint)', padding: '6px 16px', fontSize: 13, cursor: 'pointer',
            opacity: busy ? 0.5 : 1,
          }}>Cancel</button>
          <button onClick={submit} disabled={!canSubmit} style={{
            background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
            color: 'var(--accent-text)', padding: '6px 20px', fontSize: 13,
            cursor: canSubmit ? 'pointer' : 'not-allowed', opacity: canSubmit ? 1 : 0.55,
          }}>
            {busy ? 'Adding…' : 'Add'}
          </button>
        </div>
      </div>
    </div>
  )
}

function fmtSize(bytes: number): string {
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(1) + ' GB'
  if (bytes >= 1e6) return (bytes / 1e6).toFixed(1) + ' MB'
  if (bytes >= 1e3) return (bytes / 1e3).toFixed(0) + ' KB'
  return bytes.toLocaleString() + ' B'
}

const noticeStyle: React.CSSProperties = {
  fontSize: 12,
  color: 'var(--danger)',
  background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
  border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
  borderRadius: 6,
  padding: '8px 9px',
  overflowWrap: 'anywhere',
}
