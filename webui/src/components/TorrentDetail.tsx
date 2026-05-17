import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import type { TorrentSummary } from '../api/client'

function fmtSize(bytes: number): string {
  if (bytes >= 1e12) return (bytes / 1e12).toFixed(2) + ' TB'
  if (bytes >= 1e9)  return (bytes / 1e9).toFixed(2) + ' GB'
  if (bytes >= 1e6)  return (bytes / 1e6).toFixed(1) + ' MB'
  return (bytes / 1e3).toFixed(0) + ' KB'
}

function fmtDate(ts: number): string {
  if (!ts) return '—'
  return new Date(ts * 1000).toLocaleString()
}

function fmtSpeed(bps: number): string {
  if (!bps) return '0 B/s'
  return `${fmtSize(bps)}/s`
}

interface Props {
  torrent: TorrentSummary
  onClose: () => void
  autoDisplay: boolean
  onAutoDisplayChange: (enabled: boolean) => void
}

const LABEL: React.CSSProperties = {
  fontSize: 10, fontWeight: 600, letterSpacing: '0.06em',
  textTransform: 'uppercase' as const, color: 'var(--faint)', marginBottom: 2,
}
const VALUE: React.CSSProperties = {
  fontSize: 12, color: 'var(--text)', wordBreak: 'break-all' as const, marginBottom: 12,
}
const MONO: React.CSSProperties = {
  ...VALUE, fontFamily: 'monospace', fontSize: 11, color: 'var(--muted)',
}

function ActionBtn({
  label, color, onClick, disabled,
}: { label: string; color: string; onClick: () => void; disabled?: boolean }) {
  return (
    <button onClick={onClick} disabled={disabled} style={{
      background: 'var(--surface-2)',
      border: `1px solid ${color}55`,
      borderRadius: 4,
      color,
      padding: '3px 9px',
      fontSize: 11,
      cursor: disabled ? 'not-allowed' : 'pointer',
      opacity: disabled ? 0.5 : 1,
    }}>{label}</button>
  )
}

export function TorrentDetail({ torrent: t, onClose, autoDisplay, onAutoDisplayChange }: Props) {
  const qc = useQueryClient()
  const [busy, setBusy] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState(false)
  const [editingPath, setEditingPath] = useState(false)
  const [savePath, setSavePath] = useState(t.directory || '')
  const [newTracker, setNewTracker] = useState('')
  const [error, setError] = useState<string | null>(null)

  const { data: trackers, isLoading: trackersLoading, error: trackersError } = useQuery({
    queryKey: ['trackers', t.hash],
    queryFn: () => api.torrents.trackers(t.hash),
    staleTime: 2_000,
    refetchInterval: 5_000,
  })

  const { data: files, isLoading: filesLoading, error: filesError } = useQuery({
    queryKey: ['files', t.hash],
    queryFn: () => api.torrents.files(t.hash),
    staleTime: 2_000,
    refetchInterval: 5_000,
  })

  async function doAction(fn: () => Promise<void>) {
    setBusy(true)
    setError(null)
    try {
      await fn()
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Torrent action failed.')
    } finally {
      setBusy(false)
    }
  }

  async function remove(deleteFiles: boolean) {
    setBusy(true)
    setError(null)
    try {
      await api.torrents.remove(t.hash, deleteFiles)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      onClose()
    } catch (err) {
      setError(err instanceof Error ? err.message : 'Delete failed.')
    } finally {
      setBusy(false)
    }
  }

  async function saveLocation() {
    const next = savePath.trim()
    if (!next) return
    await doAction(async () => {
      await api.torrents.update(t.hash, { save_path: next })
      setEditingPath(false)
    })
  }

  async function addTracker() {
    const url = newTracker.trim()
    if (!url) return
    await doAction(async () => {
      await api.torrents.patchTrackers(t.hash, { add: [url] })
      setNewTracker('')
      qc.invalidateQueries({ queryKey: ['trackers', t.hash] })
    })
  }

  async function removeTracker(url: string) {
    await doAction(async () => {
      await api.torrents.patchTrackers(t.hash, { remove: [url] })
      qc.invalidateQueries({ queryKey: ['trackers', t.hash] })
    })
  }

  const progress = t.size_bytes > 0 ? (t.bytes_done / t.size_bytes) * 100 : 0
  const ratio = (t.ratio / 1000).toFixed(3)
  const tags = t.tags ? t.tags.split(',').filter(Boolean) : []
  const isRunning = t.is_open && t.is_active
  const state = t.message && !t.is_active
    ? { label: 'Error', color: 'var(--danger)' }
    : !t.is_open
      ? { label: 'Stopped', color: 'var(--faint)' }
      : t.complete && t.is_active
        ? { label: 'Seeding', color: 'var(--success)' }
        : !t.complete && t.is_active
          ? { label: 'Downloading', color: 'var(--accent)' }
          : { label: 'Queued', color: 'var(--muted)' }

  return (
    <aside className="torrent-detail" style={{
      width: 340, background: 'var(--bg)', borderLeft: '1px solid var(--border)',
      display: 'flex', flexDirection: 'column', flexShrink: 0, fontSize: 12,
    }}>
      {/* Header */}
      <div style={{
        padding: '10px 14px', borderBottom: '1px solid var(--border)',
        display: 'flex', alignItems: 'flex-start', gap: 8,
      }}>
        <div style={{ flex: 1, minWidth: 0 }}>
          <span style={{ display: 'block', fontWeight: 700, fontSize: 13, color: 'var(--text)', lineHeight: 1.3, overflowWrap: 'anywhere' }}>
            {t.name}
          </span>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6, marginTop: 7 }}>
            <Pill color={state.color}>{state.label}</Pill>
            {t.complete && <Pill color="var(--success)">Complete</Pill>}
            {t.down_rate > 0 && <Pill color="var(--accent)">DL {fmtSpeed(t.down_rate)}</Pill>}
            {t.up_rate > 0 && <Pill color="var(--success)">UL {fmtSpeed(t.up_rate)}</Pill>}
          </div>
        </div>
        <button onClick={onClose} title="Hide details" aria-label="Hide details" style={{
          background: 'var(--surface-2)', border: '1px solid var(--border-strong)', borderRadius: 5,
          cursor: 'pointer', color: 'var(--muted)', fontSize: 16, lineHeight: 1,
          width: 28, height: 28, padding: 0, flexShrink: 0,
        }}>×</button>
      </div>

      <label style={{
        padding: '8px 14px', borderBottom: '1px solid var(--border)',
        display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12,
        color: 'var(--muted)', fontSize: 11,
      }}>
        <span>Auto-display details on selection</span>
        <input
          type="checkbox"
          checked={autoDisplay}
          onChange={e => onAutoDisplayChange(e.target.checked)}
          style={{ accentColor: 'var(--accent)', cursor: 'pointer', flexShrink: 0 }}
        />
      </label>

      <div style={{
        padding: '6px 14px', borderBottom: '1px solid var(--border)',
        display: 'flex', justifyContent: 'flex-end',
      }}>
        <button onClick={onClose} style={{
          background: 'transparent', border: '1px solid var(--border-strong)', borderRadius: 5,
          color: 'var(--muted)', padding: '3px 9px', fontSize: 11, cursor: 'pointer',
        }}>Hide panel</button>
      </div>

      {/* Action buttons */}
      <div style={{
        padding: '8px 14px', borderBottom: '1px solid var(--border)',
        display: 'flex', flexWrap: 'wrap', gap: 6,
      }}>
        {isRunning
          ? <ActionBtn label="Stop"      color="var(--muted)" disabled={busy} onClick={() => doAction(() => api.torrents.stop(t.hash))} />
          : <ActionBtn label="Start"     color="var(--success)" disabled={busy} onClick={() => doAction(() => api.torrents.start(t.hash))} />
        }
        <ActionBtn label="Recheck"    color="var(--warning)" disabled={busy} onClick={() => doAction(() => api.torrents.recheck(t.hash))} />
        <ActionBtn label="Reannounce" color="var(--accent)" disabled={busy} onClick={() => doAction(() => api.torrents.reannounce(t.hash))} />
        <ActionBtn label="Delete"     color="var(--danger)" disabled={busy} onClick={() => setConfirmDelete(true)} />
      </div>

      {/* Delete confirmation */}
      {error && (
        <div style={{
          padding: '7px 14px', borderBottom: '1px solid var(--danger)',
          background: 'color-mix(in srgb, var(--danger) 10%, var(--panel))',
          color: 'var(--danger)', fontSize: 11, overflowWrap: 'anywhere',
        }}>
          {error}
        </div>
      )}

      {confirmDelete && (
        <div style={{
          padding: '10px 14px', background: 'color-mix(in srgb, var(--danger) 12%, var(--panel))', borderBottom: '1px solid var(--danger)',
          fontSize: 12,
        }}>
          <div style={{ color: 'var(--danger)', marginBottom: 8 }}>Delete "{t.name}"?</div>
          <div style={{ display: 'flex', gap: 6 }}>
            <button disabled={busy} onClick={() => remove(false)} style={{
              background: 'color-mix(in srgb, var(--danger) 18%, var(--surface-2))', border: 'none', borderRadius: 4,
              color: 'var(--danger)', padding: '3px 10px', fontSize: 11,
              cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.55 : 1,
            }}>Remove torrent</button>
            <button disabled={busy} onClick={() => remove(true)} style={{
              background: 'color-mix(in srgb, var(--danger) 28%, var(--surface-2))', border: 'none', borderRadius: 4,
              color: 'var(--danger)', padding: '3px 10px', fontSize: 11,
              cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.55 : 1,
            }}>+ Delete files</button>
            <button disabled={busy} onClick={() => setConfirmDelete(false)} style={{
              background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
              color: 'var(--faint)', padding: '3px 8px', fontSize: 11,
              cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.55 : 1,
            }}>Cancel</button>
          </div>
        </div>
      )}

      {/* Scrollable body */}
      <div style={{ flex: 1, overflowY: 'auto', padding: '14px 14px 0' }}>

        {/* Progress bar */}
        <div style={{ marginBottom: 14 }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
            <span style={{ fontSize: 11, color: 'var(--faint)' }}>Progress</span>
            <span style={{ fontSize: 11, color: 'var(--muted)' }}>{progress.toFixed(1)}%</span>
          </div>
          <div style={{ height: 4, background: 'var(--surface-2)', borderRadius: 2, overflow: 'hidden' }}>
            <div style={{
              width: `${progress}%`, height: '100%',
              background: progress >= 100 ? 'var(--success)' : 'var(--accent)', borderRadius: 2,
            }} />
          </div>
        </div>

        {/* Stats grid */}
        <div className="rtng-two-col" style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: 8, marginBottom: 14 }}>
          {[
            ['Size',        fmtSize(t.size_bytes)],
            ['Downloaded',  fmtSize(t.bytes_done)],
            ['Uploaded',    fmtSize(t.up_total)],
            ['Ratio',       ratio],
            ['Seeds',       String(t.peers_complete)],
            ['Peers',       String(t.peers_connected)],
            ['Added',       fmtDate(t.creation_date)],
            ['Completed',   fmtDate(t.timestamp_finished)],
          ].map(([lbl, val]) => (
            <div key={lbl} style={{
              minWidth: 0, background: 'var(--surface)', border: '1px solid var(--border)',
              borderRadius: 6, padding: '7px 8px',
            }}>
              <div style={LABEL}>{lbl}</div>
              <div style={{ ...VALUE, marginBottom: 0, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={val}>{val}</div>
            </div>
          ))}
        </div>

        {t.category && (<>
          <div style={LABEL}>Category</div>
          <div style={VALUE}>{t.category}</div>
        </>)}

        {tags.length > 0 && (<>
          <div style={LABEL}>Tags</div>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 4, marginBottom: 12 }}>
            {tags.map(tag => (
              <span key={tag} style={{
                background: 'var(--surface-2)', color: 'var(--muted)',
                padding: '1px 7px', borderRadius: 10, fontSize: 11,
              }}>{tag}</span>
            ))}
          </div>
        </>)}

        <div style={{ display: 'flex', alignItems: 'center', gap: 8 }}>
          <div style={{ ...LABEL, flex: 1 }}>Save path</div>
          {!editingPath && (
            <button
              onClick={() => {
                setSavePath(t.directory || '')
                setEditingPath(true)
              }}
              style={{
                background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
                color: 'var(--faint)', padding: '1px 6px', fontSize: 10, cursor: 'pointer',
              }}
            >
              Edit
            </button>
          )}
        </div>
        {editingPath ? (
          <div style={{ marginBottom: 12 }}>
            <input
              value={savePath}
              onChange={e => setSavePath(e.target.value)}
              disabled={busy}
              style={{
                width: '100%', boxSizing: 'border-box', background: 'var(--surface)',
                border: '1px solid var(--border-strong)', borderRadius: 4, color: 'var(--text)',
                padding: '5px 7px', fontSize: 11, fontFamily: 'monospace',
              }}
            />
            <div style={{ display: 'flex', justifyContent: 'flex-end', gap: 6, marginTop: 6 }}>
              <button
                onClick={() => setEditingPath(false)}
                disabled={busy}
                style={{
                  background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
                  color: 'var(--faint)', padding: '3px 8px', fontSize: 11, cursor: 'pointer',
                }}
              >
                Cancel
              </button>
              <button
                onClick={saveLocation}
                disabled={busy || !savePath.trim()}
                style={{
                  background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 4,
                  color: 'var(--accent-text)', padding: '3px 10px', fontSize: 11,
                  cursor: busy || !savePath.trim() ? 'not-allowed' : 'pointer',
                  opacity: busy || !savePath.trim() ? 0.5 : 1,
                }}
              >
                Save
              </button>
            </div>
          </div>
        ) : (
          <div style={{ ...MONO, marginBottom: 12 }}>{t.directory || '—'}</div>
        )}

        {t.message && (<>
          <div style={LABEL}>Message</div>
          <div style={{ ...VALUE, color: 'var(--warning)' }}>{t.message}</div>
        </>)}

        <div style={LABEL}>Hash</div>
        <div style={MONO}>{t.hash}</div>

        {/* Trackers */}
        {trackersLoading && (
          <Section title="Trackers">
            <LoadingBlock />
          </Section>
        )}
        {trackersError && (
          <Section title="Trackers">
            <Notice>Tracker details unavailable.</Notice>
          </Section>
        )}
        {trackers && (
          <Section title="Trackers">
            <div style={{ display: 'flex', gap: 6, marginBottom: 10 }}>
              <input
                value={newTracker}
                onChange={e => setNewTracker(e.target.value)}
                disabled={busy}
                placeholder="udp://tracker.example/announce"
                style={{
                  flex: 1, minWidth: 0, background: 'var(--surface)',
                  border: '1px solid var(--border-strong)', borderRadius: 4, color: 'var(--text)',
                  padding: '5px 7px', fontSize: 11, fontFamily: 'monospace',
                }}
              />
              <button
                onClick={addTracker}
                disabled={busy || !newTracker.trim()}
                style={{
                  background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 4,
                  color: 'var(--accent-text)', padding: '3px 9px', fontSize: 11,
                  cursor: busy || !newTracker.trim() ? 'not-allowed' : 'pointer',
                  opacity: busy || !newTracker.trim() ? 0.5 : 1,
                }}
              >
                Add
              </button>
            </div>
            {trackers.map((tr, i) => {
              const ok = !tr.message
              return (
              <div key={i} style={{
                marginBottom: 8, border: '1px solid var(--border)', borderRadius: 6,
                padding: 8, background: ok ? 'var(--surface)' : 'color-mix(in srgb, var(--warning) 8%, var(--surface))',
              }}>
                <div style={{ display: 'flex', gap: 6, alignItems: 'center' }}>
                  <span style={{
                    width: 7, height: 7, borderRadius: '50%', flexShrink: 0,
                    background: ok ? 'var(--success)' : 'var(--warning)',
                  }} />
                  <div style={{
                    flex: 1, minWidth: 0, fontSize: 11, color: 'var(--muted)', overflow: 'hidden',
                    textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontFamily: 'monospace',
                  }} title={tr.url}>{tr.url}</div>
                  <button
                    onClick={() => removeTracker(tr.url)}
                    disabled={busy}
                    style={{
                      background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
                      color: 'var(--faint)', padding: '1px 6px', fontSize: 10,
                      cursor: busy ? 'not-allowed' : 'pointer', opacity: busy ? 0.5 : 1,
                    }}
                  >
                    Remove
                  </button>
                </div>
                <div style={{ fontSize: 10, color: 'var(--faint)', marginTop: 1 }}>
                  {tr.scrape_complete} seeds · {tr.scrape_incomplete} peers
                  {tr.message ? ` · ${tr.message}` : ''}
                </div>
              </div>
              )
            })}
            {trackers.length === 0 && (
              <EmptyBlock>No trackers configured.</EmptyBlock>
            )}
          </Section>
        )}

        {/* Files */}
        {filesLoading && (
          <Section title="Files">
            <LoadingBlock />
          </Section>
        )}
        {filesError && (
          <Section title="Files">
            <Notice>File details unavailable.</Notice>
          </Section>
        )}
        {files && files.length > 0 && (
          <Section title={`Files (${files.length})`}>
            {files.map(f => {
              const fp = f.size_chunks > 0 ? (f.completed_chunks / f.size_chunks) * 100 : 100
              const priority = f.priority === 0 ? 'Skipped' : f.priority >= 2 ? 'High' : 'Normal'
              return (
                <div key={f.index} style={{
                  marginBottom: 8, border: '1px solid var(--border)', borderRadius: 6,
                  background: 'var(--surface)', padding: 7,
                }}>
                  <div style={{
                    fontSize: 11, color: 'var(--muted)', overflow: 'hidden',
                    textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }} title={f.path}>{f.path.split('/').pop()}</div>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 2 }}>
                    <div style={{ flex: 1, height: 2, background: 'var(--surface-2)', borderRadius: 1, overflow: 'hidden' }}>
                      <div style={{ width: `${fp}%`, height: '100%', background: fp >= 100 ? 'var(--success)' : 'var(--accent)' }} />
                    </div>
                    <span style={{ fontSize: 10, color: 'var(--faint)', flexShrink: 0 }}>{fmtSize(f.size_bytes)}</span>
                    <span style={{ fontSize: 10, color: f.priority === 0 ? 'var(--faint)' : f.priority >= 2 ? 'var(--accent)' : 'var(--success)', flexShrink: 0 }}>{priority}</span>
                  </div>
                </div>
              )
            })}
          </Section>
        )}
        {files && files.length === 0 && (
          <Section title="Files">
            <EmptyBlock>No file list available yet.</EmptyBlock>
          </Section>
        )}

        <div style={{ height: 14 }} />
      </div>
    </aside>
  )
}

function Pill({ color, children }: { color: string; children: React.ReactNode }) {
  return (
    <span style={{
      display: 'inline-flex', alignItems: 'center', minHeight: 18,
      border: `1px solid color-mix(in srgb, ${color} 42%, transparent)`,
      background: `color-mix(in srgb, ${color} 12%, transparent)`,
      color,
      borderRadius: 999,
      padding: '0 7px',
      fontSize: 10,
      fontWeight: 700,
      whiteSpace: 'nowrap',
    }}>{children}</span>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: 16 }}>
      <div style={{
        fontSize: 10, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase',
        color: 'var(--accent)', borderBottom: '1px solid var(--border)', paddingBottom: 4, marginBottom: 8,
      }}>{title}</div>
      {children}
    </div>
  )
}

function LoadingBlock() {
  return (
    <div style={{
      border: '1px solid var(--border)', borderRadius: 6, background: 'var(--surface)',
      padding: 9, display: 'grid', gap: 7,
    }}>
      <span className="rtng-skeleton" style={{ width: '58%', height: 10 }} />
      <span className="rtng-skeleton" style={{ width: '82%', height: 8 }} />
    </div>
  )
}

function EmptyBlock({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      border: '1px dashed var(--border-strong)', borderRadius: 6,
      background: 'color-mix(in srgb, var(--surface) 70%, transparent)',
      color: 'var(--faint)', fontSize: 11, padding: '9px 10px', marginBottom: 8,
    }}>{children}</div>
  )
}

function Notice({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
      borderRadius: 6,
      background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
      color: 'var(--danger)',
      fontSize: 11,
      padding: '8px 9px',
      marginBottom: 8,
      overflowWrap: 'anywhere',
    }}>{children}</div>
  )
}
