import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type TorrentSummary, type TorrentFile, type Tracker } from '../api/client'

type Tab = 'general' | 'trackers' | 'files' | 'limits'

interface Props {
  torrent: TorrentSummary
  onClose: () => void
}

const INPUT: React.CSSProperties = {
  width: '100%', background: 'var(--bg)', border: '1px solid var(--border-strong)', borderRadius: 5,
  color: 'var(--text)', padding: '6px 8px', fontSize: 12, outline: 'none', boxSizing: 'border-box',
}

export function TorrentPropertiesDialog({ torrent, onClose }: Props) {
  const qc = useQueryClient()
  const [tab, setTab] = useState<Tab>('general')
  const [name, setName] = useState(torrent.name)
  const [location, setLocation] = useState(torrent.directory)
  const [category, setCategory] = useState(torrent.category)
  const [tagsText, setTagsText] = useState(torrent.tags)
  const [ratioLimit, setRatioLimit] = useState('')
  const [seedMinutes, setSeedMinutes] = useState('')
  const [newTracker, setNewTracker] = useState('')
  const [editingTracker, setEditingTracker] = useState<Tracker | null>(null)
  const [trackerUrl, setTrackerUrl] = useState('')
  const [renamingFile, setRenamingFile] = useState<TorrentFile | null>(null)
  const [fileName, setFileName] = useState('')
  const [busy, setBusy] = useState(false)
  const [message, setMessage] = useState('')

  const { data: categories = [] } = useQuery({ queryKey: ['categories'], queryFn: api.categories.list })
  const { data: allTags = [] } = useQuery({ queryKey: ['tags'], queryFn: api.tags.list })
  const { data: trackers = [] } = useQuery({
    queryKey: ['trackers', torrent.hash],
    queryFn: () => api.torrents.trackers(torrent.hash),
    staleTime: 2_000,
    refetchInterval: 5_000,
  })
  const { data: files = [] } = useQuery({
    queryKey: ['files', torrent.hash],
    queryFn: () => api.torrents.files(torrent.hash),
    staleTime: 2_000,
    refetchInterval: 5_000,
  })

  useEffect(() => {
    if (editingTracker) setTrackerUrl(editingTracker.url)
  }, [editingTracker])

  async function apply(label: string, fn: () => Promise<void>) {
    setBusy(true)
    setMessage('')
    try {
      await fn()
      setMessage(label)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      qc.invalidateQueries({ queryKey: ['trackers', torrent.hash] })
      qc.invalidateQueries({ queryKey: ['files', torrent.hash] })
      qc.invalidateQueries({ queryKey: ['tags'] })
      qc.invalidateQueries({ queryKey: ['categories'] })
    } catch (err) {
      setMessage(String(err))
    } finally {
      setBusy(false)
    }
  }

  const tags = tagsText.split(',').map(t => t.trim()).filter(Boolean)

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1200,
      display: 'grid', placeItems: 'center', padding: 22,
    }}>
      <div style={{
        width: 'min(820px, 100%)', height: 'min(680px, 90vh)', background: 'var(--panel)',
        border: '1px solid var(--border-strong)', borderRadius: 8, display: 'flex', flexDirection: 'column',
        boxShadow: '0 24px 60px var(--shadow)',
      }}>
        <header style={{ padding: '12px 14px', borderBottom: '1px solid var(--border)', display: 'flex', gap: 12 }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ color: 'var(--text)', fontWeight: 700, fontSize: 15 }}>Properties</div>
            <div style={{ color: 'var(--faint)', fontSize: 12, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{torrent.name}</div>
          </div>
          <button onClick={onClose} style={smallButton('#94a3b8')}>Close</button>
        </header>

        <div style={{ display: 'flex', minHeight: 0, flex: 1 }}>
          <nav style={{ width: 150, borderRight: '1px solid var(--border)', padding: 10 }}>
            {(['general', 'trackers', 'files', 'limits'] as Tab[]).map(item => (
              <button key={item} onClick={() => setTab(item)} style={{
                width: '100%', textAlign: 'left', marginBottom: 5, borderRadius: 5,
                background: tab === item ? 'var(--accent-soft)' : 'transparent',
                border: '1px solid ' + (tab === item ? 'var(--accent)' : 'transparent'),
                color: tab === item ? 'var(--accent-text)' : 'var(--muted)',
                padding: '7px 9px', fontSize: 12, cursor: 'pointer', textTransform: 'capitalize',
              }}>{item}</button>
            ))}
          </nav>

          <main style={{ flex: 1, overflowY: 'auto', padding: 14 }}>
            {tab === 'general' && (
              <div style={{ display: 'grid', gap: 12 }}>
                <Field label="Name">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <input value={name} onChange={e => setName(e.target.value)} style={INPUT} />
                    <button disabled={busy || !name.trim()} onClick={() => apply('Renamed', () => api.torrents.rename(torrent.hash, name.trim()))} style={smallButton('#93c5fd')}>Rename</button>
                  </div>
                </Field>
                <Field label="Location">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <input value={location} onChange={e => setLocation(e.target.value)} style={{ ...INPUT, fontFamily: 'monospace' }} />
                    <button disabled={busy || !location.trim()} onClick={() => apply('Location updated', () => api.torrents.setLocation([torrent.hash], location.trim()))} style={smallButton('#93c5fd')}>Move</button>
                  </div>
                </Field>
                <Field label="Category">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <select value={category} onChange={e => setCategory(e.target.value)} style={INPUT}>
                      <option value="">None</option>
                      {categories.map(c => <option key={c.name} value={c.name}>{c.name}</option>)}
                    </select>
                    <button disabled={busy} onClick={() => apply('Category updated', () => api.torrents.setCategory(torrent.hash, category))} style={smallButton('#93c5fd')}>Apply</button>
                  </div>
                </Field>
                <Field label="Tags">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <input value={tagsText} onChange={e => setTagsText(e.target.value)} placeholder="comma,separated,tags" style={INPUT} />
                    <button disabled={busy} onClick={() => apply('Tags updated', () => api.torrents.setTags([torrent.hash], tags))} style={smallButton('#93c5fd')}>Apply</button>
                  </div>
                  {allTags.length > 0 && (
                    <div style={{ display: 'flex', gap: 5, flexWrap: 'wrap', marginTop: 7 }}>
                      {allTags.map(tag => (
                        <button key={tag} onClick={() => setTagsText(Array.from(new Set([...tags, tag])).join(','))} style={chipButton}>{tag}</button>
                      ))}
                    </div>
                  )}
                </Field>
                <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, color: 'var(--muted)', fontSize: 12 }}>
                  <Info label="Hash" value={torrent.hash} mono />
                  <Info label="Tracker" value={torrent.tracker_url || '-'} mono />
                  <Info label="Save path" value={torrent.directory || '-'} mono />
                  <Info label="Message" value={torrent.message || '-'} />
                </div>
              </div>
            )}

            {tab === 'trackers' && (
              <div>
                <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8, marginBottom: 12 }}>
                  <input value={newTracker} onChange={e => setNewTracker(e.target.value)} placeholder="udp://tracker.example/announce" style={{ ...INPUT, fontFamily: 'monospace' }} />
                  <button disabled={busy || !newTracker.trim()} onClick={() => apply('Tracker added', async () => {
                    await api.torrents.patchTrackers(torrent.hash, { add: [newTracker.trim()] })
                    setNewTracker('')
                  })} style={smallButton('#93c5fd')}>Add</button>
                </div>
                {trackers.map(tracker => (
                  <div key={tracker.url} style={{ borderBottom: '1px solid var(--border)', padding: '8px 0' }}>
                    {editingTracker?.url === tracker.url ? (
                      <div style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7 }}>
                        <input value={trackerUrl} onChange={e => setTrackerUrl(e.target.value)} style={{ ...INPUT, fontFamily: 'monospace' }} />
                        <button onClick={() => apply('Tracker edited', async () => {
                          await api.torrents.patchTrackers(torrent.hash, { edit: [{ orig_url: tracker.url, new_url: trackerUrl.trim() }] })
                          setEditingTracker(null)
                        })} style={smallButton('#93c5fd')}>Save</button>
                        <button onClick={() => setEditingTracker(null)} style={smallButton('#64748b')}>Cancel</button>
                      </div>
                    ) : (
                      <div style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7, alignItems: 'center' }}>
                        <div>
                          <div style={{ color: 'var(--text)', fontFamily: 'monospace', fontSize: 11, overflowWrap: 'anywhere' }}>{tracker.url}</div>
                          <div style={{ color: 'var(--faint)', fontSize: 11 }}>{tracker.scrape_complete} seeds, {tracker.scrape_incomplete} peers {tracker.message ? `- ${tracker.message}` : ''}</div>
                        </div>
                        <button onClick={() => setEditingTracker(tracker)} style={smallButton('#94a3b8')}>Edit</button>
                        <button onClick={() => apply('Tracker removed', () => api.torrents.patchTrackers(torrent.hash, { remove: [tracker.url] }))} style={smallButton('#f87171')}>Remove</button>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}

            {tab === 'files' && (
              <div style={{ display: 'grid', gap: 7 }}>
                {files.map(file => <FileRow
                  key={file.index}
                  file={file}
                  busy={busy}
                  renaming={renamingFile?.index === file.index}
                  name={fileName}
                  onName={setFileName}
                  onRenameStart={() => {
                    setRenamingFile(file)
                    setFileName(file.path.split('/').pop() || file.path)
                  }}
                  onRenameCancel={() => setRenamingFile(null)}
                  onRenameSave={() => apply('File renamed', async () => {
                    await api.torrents.renameFile(torrent.hash, file.index, fileName.trim())
                    setRenamingFile(null)
                  })}
                  onPriority={(priority) => apply('File priority updated', () => api.torrents.setFilePriority(torrent.hash, [file.index], priority))}
                />)}
              </div>
            )}

            {tab === 'limits' && (
              <div style={{ display: 'grid', gap: 12, maxWidth: 420 }}>
                <Field label="Ratio limit">
                  <input value={ratioLimit} onChange={e => setRatioLimit(e.target.value)} placeholder="-2 uses default, -1 unlimited" style={INPUT} />
                </Field>
                <Field label="Seeding time limit minutes">
                  <input value={seedMinutes} onChange={e => setSeedMinutes(e.target.value)} placeholder="-2 uses default, -1 unlimited" style={INPUT} />
                </Field>
                <button disabled={busy} onClick={() => apply('Share limits updated', () => api.torrents.setShareLimits(
                  [torrent.hash],
                  ratioLimit.trim() ? Number(ratioLimit) : -2,
                  seedMinutes.trim() ? Number(seedMinutes) : -2,
                ))} style={{ ...smallButton('#93c5fd'), width: 'fit-content' }}>Apply share limits</button>
                <button disabled={busy} onClick={() => apply('Sequential toggled', () => api.torrents.toggleSequential([torrent.hash]))} style={{ ...smallButton('#f59e0b'), width: 'fit-content' }}>Toggle sequential download</button>
              </div>
            )}
          </main>
        </div>
        {message && <div style={{ borderTop: '1px solid var(--border)', padding: '8px 14px', color: message.startsWith('Error') ? '#f87171' : 'var(--muted)', fontSize: 12 }}>{message}</div>}
      </div>
    </div>
  )
}

function FileRow({ file, busy, renaming, name, onName, onRenameStart, onRenameCancel, onRenameSave, onPriority }: {
  file: TorrentFile
  busy: boolean
  renaming: boolean
  name: string
  onName: (name: string) => void
  onRenameStart: () => void
  onRenameCancel: () => void
  onRenameSave: () => void
  onPriority: (priority: number) => void
}) {
  const pct = file.size_chunks ? Math.round((file.completed_chunks / file.size_chunks) * 100) : 100
  return (
    <div style={{ border: '1px solid var(--border)', borderRadius: 5, padding: 8, background: 'var(--surface)' }}>
      {renaming ? (
        <div style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7 }}>
          <input value={name} onChange={e => onName(e.target.value)} style={INPUT} />
          <button disabled={busy || !name.trim()} onClick={onRenameSave} style={smallButton('#93c5fd')}>Save</button>
          <button disabled={busy} onClick={onRenameCancel} style={smallButton('#64748b')}>Cancel</button>
        </div>
      ) : (
        <div style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7, alignItems: 'center' }}>
          <div style={{ minWidth: 0 }}>
            <div style={{ color: 'var(--text)', fontSize: 12, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={file.path}>{file.path}</div>
            <div style={{ color: 'var(--faint)', fontSize: 11 }}>{pct}% complete, priority {file.priority}</div>
          </div>
          <select value={file.priority} disabled={busy} onChange={e => onPriority(Number(e.target.value))} style={{ ...INPUT, width: 112 }}>
            <option value={0}>Do not download</option>
            <option value={1}>Normal</option>
            <option value={2}>High</option>
          </select>
          <button disabled={busy} onClick={onRenameStart} style={smallButton('#94a3b8')}>Rename</button>
        </div>
      )}
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label style={{ display: 'grid', gap: 5, color: 'var(--faint)', fontSize: 11, fontWeight: 700, textTransform: 'uppercase' }}>{label}{children}</label>
}

function Info({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return <div><div style={{ color: 'var(--faint)', fontSize: 10, textTransform: 'uppercase', marginBottom: 3 }}>{label}</div><div style={{ fontFamily: mono ? 'monospace' : undefined, overflowWrap: 'anywhere' }}>{value}</div></div>
}

function smallButton(color: string): React.CSSProperties {
  return {
    background: 'var(--surface-2)', border: `1px solid ${color}66`, borderRadius: 5,
    color, padding: '5px 9px', fontSize: 12, cursor: 'pointer',
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
