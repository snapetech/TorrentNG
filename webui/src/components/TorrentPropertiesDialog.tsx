import { useEffect, useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type TorrentSummary, type TorrentFile, type Tracker } from '../api/client'
import { TrackerUrl } from '../lib/maskUrl'

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
  const [messageTone, setMessageTone] = useState<'ok' | 'error'>('ok')

  const { data: categories = [] } = useQuery({ queryKey: ['categories'], queryFn: api.categories.list })
  const { data: allTags = [] } = useQuery({ queryKey: ['tags'], queryFn: api.tags.list })
  const { data: trackers = [], isLoading: trackersLoading, error: trackersError } = useQuery({
    queryKey: ['trackers', torrent.hash],
    queryFn: () => api.torrents.trackers(torrent.hash),
    staleTime: 2_000,
    refetchInterval: 5_000,
  })
  const { data: files = [], isLoading: filesLoading, error: filesError } = useQuery({
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
    setMessageTone('ok')
    try {
      await fn()
      setMessage(label)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      qc.invalidateQueries({ queryKey: ['trackers', torrent.hash] })
      qc.invalidateQueries({ queryKey: ['files', torrent.hash] })
      qc.invalidateQueries({ queryKey: ['tags'] })
      qc.invalidateQueries({ queryKey: ['categories'] })
    } catch (err) {
      setMessage(err instanceof Error ? err.message : String(err))
      setMessageTone('error')
    } finally {
      setBusy(false)
    }
  }

  const tags = tagsText.split(',').map(t => t.trim()).filter(Boolean)

  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1200,
      display: 'grid', placeItems: 'center', padding: 22,
    }} onClick={e => { if (!busy && e.target === e.currentTarget) onClose() }}>
      <div role="dialog" aria-modal="true" aria-label={`Properties for ${torrent.name}`} className="tng-properties-dialog tng-modal" style={{
        width: 'min(820px, 100%)', height: 'min(680px, 90vh)', background: 'var(--panel)',
        border: '1px solid var(--border-strong)', borderRadius: 8, display: 'flex', flexDirection: 'column',
        boxShadow: '0 24px 60px var(--shadow)',
      }} onClick={e => e.stopPropagation()}>
        <header style={{ padding: '12px 14px', borderBottom: '1px solid var(--border)', display: 'flex', gap: 12 }}>
          <div style={{ flex: 1, minWidth: 0 }}>
            <div style={{ color: 'var(--text)', fontWeight: 700, fontSize: 15 }}>Properties</div>
            <div style={{ color: 'var(--faint)', fontSize: 12, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{torrent.name}</div>
          </div>
          {busy && <span style={{
            alignSelf: 'center', color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
            borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700,
          }}>Saving</span>}
          <button onClick={onClose} disabled={busy} style={smallButton('#94a3b8', busy)}>Close</button>
        </header>

        <div className="tng-properties-body" style={{ display: 'flex', minHeight: 0, flex: 1 }}>
          <nav className="tng-properties-tabs" style={{ width: 150, borderRight: '1px solid var(--border)', padding: 10 }}>
            {(['general', 'trackers', 'files', 'limits'] as Tab[]).map(item => (
              <button key={item} onClick={() => setTab(item)} disabled={busy} style={{
                width: '100%', textAlign: 'left', marginBottom: 5, borderRadius: 5,
                background: tab === item ? 'var(--accent-soft)' : 'transparent',
                border: '1px solid ' + (tab === item ? 'var(--accent)' : 'transparent'),
                color: tab === item ? 'var(--accent-text)' : 'var(--muted)',
                padding: '7px 9px', fontSize: 12, cursor: busy ? 'not-allowed' : 'pointer',
                textTransform: 'capitalize', opacity: busy && tab !== item ? 0.55 : 1,
                display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8,
              }}>
                <span>{item}</span>
                {item === 'trackers' && trackers.length > 0 && <TabCount>{trackers.length}</TabCount>}
                {item === 'files' && files.length > 0 && <TabCount>{files.length}</TabCount>}
              </button>
            ))}
          </nav>

          <main className="tng-properties-main" style={{ flex: 1, overflowY: 'auto', padding: 14 }}>
            {tab === 'general' && (
              <div style={{ display: 'grid', gap: 12 }}>
                <Field label="Name">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <input value={name} onChange={e => setName(e.target.value)} disabled={busy} style={INPUT} />
                    <button disabled={busy || !name.trim()} onClick={() => apply('Renamed', () => api.torrents.rename(torrent.hash, name.trim()))} style={smallButton('#93c5fd', busy || !name.trim())}>Rename</button>
                  </div>
                </Field>
                <Field label="Location">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <input value={location} onChange={e => setLocation(e.target.value)} disabled={busy} style={{ ...INPUT, fontFamily: 'monospace' }} />
                    <button disabled={busy || !location.trim()} onClick={() => apply('Location updated', () => api.torrents.setLocation([torrent.hash], location.trim()))} style={smallButton('#93c5fd', busy || !location.trim())}>Move</button>
                  </div>
                </Field>
                <Field label="Category">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <select value={category} onChange={e => setCategory(e.target.value)} disabled={busy} style={INPUT}>
                      <option value="">None</option>
                      {categories.map(c => <option key={c.name} value={c.name}>{c.name}</option>)}
                    </select>
                    <button disabled={busy} onClick={() => apply('Category updated', () => api.torrents.setCategory(torrent.hash, category))} style={smallButton('#93c5fd', busy)}>Apply</button>
                  </div>
                </Field>
                <Field label="Tags">
                  <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8 }}>
                    <input value={tagsText} onChange={e => setTagsText(e.target.value)} disabled={busy} placeholder="comma,separated,tags" style={INPUT} />
                    <button disabled={busy} onClick={() => apply('Tags updated', () => api.torrents.setTags([torrent.hash], tags))} style={smallButton('#93c5fd', busy)}>Apply</button>
                  </div>
                  {allTags.length > 0 && (
                    <div style={{ display: 'flex', gap: 5, flexWrap: 'wrap', marginTop: 7 }}>
                      {allTags.map(tag => (
                        <button
                          key={tag}
                          className="tng-tag-chip"
                          data-active={tags.includes(tag) ? 'true' : 'false'}
                          disabled={busy}
                          onClick={() => setTagsText(Array.from(new Set([...tags, tag])).join(','))}
                          style={{ ...chipButton, opacity: busy ? 0.55 : 1, cursor: busy ? 'not-allowed' : 'pointer' }}
                        >{tag}</button>
                      ))}
                    </div>
                  )}
                </Field>
                <div className="tng-two-col" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 10, color: 'var(--muted)', fontSize: 12 }}>
                  <Info label="Hash" value={torrent.hash} mono />
                  <div className="tng-metric-tile" style={{ border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)', padding: 9 }}>
                    <div style={{ color: 'var(--faint)', fontSize: 10, textTransform: 'uppercase', marginBottom: 3 }}>Tracker</div>
                    <div style={{ overflowWrap: 'anywhere' }}>{torrent.tracker_url ? <TrackerUrl url={torrent.tracker_url} /> : '-'}</div>
                  </div>
                  <Info label="Save path" value={torrent.directory || '-'} mono />
                  <Info label="Message" value={torrent.message || '-'} />
                </div>
              </div>
            )}

            {tab === 'trackers' && (
              <div>
                <div style={{ display: 'grid', gridTemplateColumns: '1fr auto', gap: 8, marginBottom: 12 }}>
                  <input value={newTracker} onChange={e => setNewTracker(e.target.value)} disabled={busy} placeholder="udp://tracker.example/announce" style={{ ...INPUT, fontFamily: 'monospace' }} />
                  <button disabled={busy || !newTracker.trim()} onClick={() => apply('Tracker added', async () => {
                    await api.torrents.patchTrackers(torrent.hash, { add: [newTracker.trim()] })
                    setNewTracker('')
                  })} style={smallButton('#93c5fd', busy || !newTracker.trim())}>Add</button>
                </div>
                {trackersLoading && <LoadingRows count={2} />}
                {trackersError && <Notice>Tracker details unavailable.</Notice>}
                {!trackersLoading && !trackersError && trackers.length === 0 && (
                  <EmptyState>No trackers configured.</EmptyState>
                )}
                {trackers.map(tracker => (
                  <div
                    key={tracker.url}
                    className="tng-properties-row"
                    data-tone={tracker.message ? 'warn' : 'ok'}
                    style={{
                      border: '1px solid var(--border)', borderRadius: 7, padding: 9,
                      marginBottom: 7, background: tracker.message ? 'color-mix(in srgb, var(--warning) 7%, var(--surface))' : 'var(--surface)',
                    }}
                  >
                    {editingTracker?.url === tracker.url ? (
                      <div className="tng-action-grid-3" style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7 }}>
                        <input value={trackerUrl} onChange={e => setTrackerUrl(e.target.value)} disabled={busy} style={{ ...INPUT, fontFamily: 'monospace' }} />
                        <button disabled={busy || !trackerUrl.trim()} onClick={() => apply('Tracker edited', async () => {
                          await api.torrents.patchTrackers(torrent.hash, { edit: [{ orig_url: tracker.url, new_url: trackerUrl.trim() }] })
                          setEditingTracker(null)
                        })} style={smallButton('#93c5fd', busy || !trackerUrl.trim())}>Save</button>
                        <button disabled={busy} onClick={() => setEditingTracker(null)} style={smallButton('#64748b', busy)}>Cancel</button>
                      </div>
                    ) : (
                      <div className="tng-action-grid-3" style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7, alignItems: 'center' }}>
                        <div>
                          <div style={{ color: 'var(--text)', fontSize: 11, overflowWrap: 'anywhere' }}><TrackerUrl url={tracker.url} /></div>
                          <div style={{ color: 'var(--faint)', fontSize: 11 }}>{tracker.scrape_complete} seeds, {tracker.scrape_incomplete} peers {tracker.message ? `- ${tracker.message}` : ''}</div>
                        </div>
                        <button disabled={busy} onClick={() => setEditingTracker(tracker)} style={smallButton('#94a3b8', busy)}>Edit</button>
                        <button disabled={busy} onClick={() => apply('Tracker removed', () => api.torrents.patchTrackers(torrent.hash, { remove: [tracker.url] }))} style={smallButton('#f87171', busy)}>Remove</button>
                      </div>
                    )}
                  </div>
                ))}
              </div>
            )}

            {tab === 'files' && (
              <div style={{ display: 'grid', gap: 7 }}>
                {filesLoading && <LoadingRows count={3} />}
                {filesError && <Notice>File details unavailable.</Notice>}
                {!filesLoading && !filesError && files.length === 0 && (
                  <EmptyState>No files reported.</EmptyState>
                )}
                {files.map(file => <FileRow
                  key={file.index}
                  file={file}
                  torrentComplete={torrent.complete}
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
                  <input value={ratioLimit} onChange={e => setRatioLimit(e.target.value)} disabled={busy} placeholder="-2 uses default, -1 unlimited" style={INPUT} />
                </Field>
                <Field label="Seeding time limit minutes">
                  <input value={seedMinutes} onChange={e => setSeedMinutes(e.target.value)} disabled={busy} placeholder="-2 uses default, -1 unlimited" style={INPUT} />
                </Field>
                <button disabled={busy} onClick={() => apply('Share limits updated', () => api.torrents.setShareLimits(
                  [torrent.hash],
                  ratioLimit.trim() ? Number(ratioLimit) : -2,
                  seedMinutes.trim() ? Number(seedMinutes) : -2,
                ))} style={{ ...smallButton('#93c5fd', busy), width: 'fit-content' }}>Apply share limits</button>
                <button disabled={busy} onClick={() => apply('Sequential toggled', () => api.torrents.toggleSequential([torrent.hash]))} style={{ ...smallButton('#f59e0b', busy), width: 'fit-content' }}>Toggle sequential download</button>
              </div>
            )}
          </main>
        </div>
        {message && <div role={messageTone === 'error' ? 'alert' : 'status'} style={{
          borderTop: '1px solid var(--border)', padding: '8px 14px',
          color: messageTone === 'error' ? 'var(--danger)' : 'var(--success)', fontSize: 12,
          background: messageTone === 'error' ? 'color-mix(in srgb, var(--danger) 8%, transparent)' : 'color-mix(in srgb, var(--success) 7%, transparent)',
          overflowWrap: 'anywhere',
        }}>{message}</div>}
      </div>
    </div>
  )
}

function FileRow({ file, torrentComplete, busy, renaming, name, onName, onRenameStart, onRenameCancel, onRenameSave, onPriority }: {
  file: TorrentFile
  torrentComplete: boolean
  busy: boolean
  renaming: boolean
  name: string
  onName: (name: string) => void
  onRenameStart: () => void
  onRenameCancel: () => void
  onRenameSave: () => void
  onPriority: (priority: number) => void
}) {
  // A wanted file (priority !== 0) in a torrent the backend already reports
  // as fully complete can't itself be incomplete - trust that over a
  // stale/uninitialized per-file chunk count (observed showing 0% for files
  // that are actually done).
  const pct = torrentComplete && file.priority !== 0
    ? 100
    : file.size_chunks ? Math.round((file.completed_chunks / file.size_chunks) * 100) : 100
  const priority = file.priority === 0
    ? { label: 'Skipped', color: 'var(--faint)' }
    : file.priority >= 2
      ? { label: 'High', color: 'var(--accent)' }
      : { label: 'Normal', color: 'var(--success)' }
  return (
    <div
      className="tng-properties-row"
      data-tone={file.priority === 0 ? 'muted' : pct >= 100 ? 'ok' : 'active'}
      style={{ border: '1px solid var(--border)', borderRadius: 7, padding: 9, background: 'var(--surface)' }}
    >
      {renaming ? (
        <div className="tng-action-grid-3" style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7 }}>
          <input value={name} onChange={e => onName(e.target.value)} style={INPUT} />
          <button disabled={busy || !name.trim()} onClick={onRenameSave} style={smallButton('#93c5fd', busy || !name.trim())}>Save</button>
          <button disabled={busy} onClick={onRenameCancel} style={smallButton('#64748b', busy)}>Cancel</button>
        </div>
      ) : (
        <div className="tng-action-grid-3" style={{ display: 'grid', gridTemplateColumns: '1fr auto auto', gap: 7, alignItems: 'center' }}>
          <div style={{ minWidth: 0 }}>
            <div style={{ color: 'var(--text)', fontSize: 12, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={file.path}>{file.path}</div>
            <div style={{ display: 'flex', alignItems: 'center', gap: 8, marginTop: 4 }}>
              <div style={{ flex: 1, height: 4, background: 'var(--surface-2)', borderRadius: 999, overflow: 'hidden' }}>
                <div style={{
                  width: `${Math.min(100, Math.max(0, pct))}%`, height: '100%',
                  background: pct >= 100 ? 'var(--success)' : 'var(--accent)',
                }} />
              </div>
              <span style={{ color: 'var(--faint)', fontSize: 10, flexShrink: 0 }}>{pct}%</span>
              <span style={{
                color: priority.color, fontSize: 10, fontWeight: 700,
                border: '1px solid var(--border)', borderRadius: 999, padding: '0 6px',
                background: 'var(--bg)',
              }}>{priority.label}</span>
            </div>
          </div>
          <select value={file.priority} disabled={busy} onChange={e => onPriority(Number(e.target.value))} style={{ ...INPUT, width: 112 }}>
            <option value={0}>Do not download</option>
            <option value={1}>Normal</option>
            <option value={2}>High</option>
          </select>
          <button disabled={busy} onClick={onRenameStart} style={smallButton('#94a3b8', busy)}>Rename</button>
        </div>
      )}
    </div>
  )
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label className="tng-form-card" style={{
    display: 'grid', gap: 6, color: 'var(--faint)', fontSize: 11, fontWeight: 700,
    textTransform: 'uppercase', background: 'var(--surface)', border: '1px solid var(--border)',
    borderRadius: 7, padding: 10,
  }}>{label}{children}</label>
}

function LoadingRows({ count }: { count: number }) {
  return (
    <div style={{ display: 'grid', gap: 7 }}>
      {Array.from({ length: count }, (_, index) => (
        <div key={index} style={{
          border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)',
          padding: '9px 10px', display: 'grid', gap: 7,
        }}>
          <span className="tng-skeleton" style={{ width: index % 2 ? '46%' : '64%', height: 10 }} />
          <span className="tng-skeleton" style={{ width: index % 2 ? '76%' : '52%', height: 8 }} />
        </div>
      ))}
    </div>
  )
}

function EmptyState({ children }: { children: React.ReactNode }) {
  return <div style={{
    color: 'var(--faint)', fontSize: 12,
    border: '1px dashed var(--border-strong)', borderRadius: 7,
    background: 'color-mix(in srgb, var(--surface) 72%, transparent)',
    padding: '12px 13px',
  }}>{children}</div>
}

function Notice({ children }: { children: React.ReactNode }) {
  return <div role="alert" style={{
    color: 'var(--danger)', fontSize: 12,
    background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
    border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
    borderRadius: 6,
    padding: '8px 9px',
    overflowWrap: 'anywhere',
  }}>{children}</div>
}

function TabCount({ children }: { children: React.ReactNode }) {
  return (
    <span style={{
      border: '1px solid var(--border)', borderRadius: 999, padding: '0 6px',
      background: 'var(--surface)', color: 'var(--faint)', fontSize: 10,
    }}>{children}</span>
  )
}

function Info({ label, value, mono }: { label: string; value: string; mono?: boolean }) {
  return <div className="tng-metric-tile" style={{ border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)', padding: 9 }}>
    <div style={{ color: 'var(--faint)', fontSize: 10, textTransform: 'uppercase', marginBottom: 3 }}>{label}</div>
    <div style={{ fontFamily: mono ? 'monospace' : undefined, overflowWrap: 'anywhere' }}>{value}</div>
  </div>
}

function smallButton(color: string, disabled = false): React.CSSProperties {
  return {
    background: 'var(--surface-2)', border: `1px solid ${color}66`, borderRadius: 5,
    color, padding: '5px 9px', fontSize: 12,
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
