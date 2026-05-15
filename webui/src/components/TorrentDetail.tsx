import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import type { TorrentSummary } from '../api/client'

const BASE = '/api/v1'

interface Tracker {
  url: string
  is_enabled: boolean
  success_counter: number
  failed_counter: number
  scrape_complete: number
  scrape_incomplete: number
  message: string
}

interface TorrentFile {
  index: number
  path: string
  size_bytes: number
  completed_chunks: number
  size_chunks: number
  priority: number
  is_created: boolean
}

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

async function apiPost(path: string): Promise<void> {
  await fetch(BASE + path, { method: 'POST', headers: { 'X-RTNG-CSRF': '1' } })
}

async function apiDelete(path: string): Promise<void> {
  await fetch(BASE + path, { method: 'DELETE', headers: { 'X-RTNG-CSRF': '1' } })
}

async function fetchTrackers(hash: string): Promise<Tracker[]> {
  const res = await fetch(`${BASE}/torrents/${hash}/trackers`)
  if (!res.ok) throw new Error('trackers')
  const data = await res.json()
  return data.trackers as Tracker[]
}

async function fetchFiles(hash: string): Promise<TorrentFile[]> {
  const res = await fetch(`${BASE}/torrents/${hash}/files`)
  if (!res.ok) throw new Error('files')
  const data = await res.json()
  return data.files as TorrentFile[]
}

interface Props {
  torrent: TorrentSummary
  onClose: () => void
}

const LABEL: React.CSSProperties = {
  fontSize: 10, fontWeight: 600, letterSpacing: '0.06em',
  textTransform: 'uppercase' as const, color: '#475569', marginBottom: 2,
}
const VALUE: React.CSSProperties = {
  fontSize: 12, color: '#cbd5e1', wordBreak: 'break-all' as const, marginBottom: 12,
}
const MONO: React.CSSProperties = {
  ...VALUE, fontFamily: 'monospace', fontSize: 11, color: '#94a3b8',
}

function ActionBtn({
  label, color, onClick, disabled,
}: { label: string; color: string; onClick: () => void; disabled?: boolean }) {
  return (
    <button onClick={onClick} disabled={disabled} style={{
      background: '#1e2433',
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

export function TorrentDetail({ torrent: t, onClose }: Props) {
  const qc = useQueryClient()
  const [busy, setBusy] = useState(false)
  const [confirmDelete, setConfirmDelete] = useState(false)

  const { data: trackers } = useQuery({
    queryKey: ['trackers', t.hash],
    queryFn: () => fetchTrackers(t.hash),
    staleTime: 10_000,
  })

  const { data: files } = useQuery({
    queryKey: ['files', t.hash],
    queryFn: () => fetchFiles(t.hash),
    staleTime: 30_000,
  })

  async function action(path: string) {
    setBusy(true)
    try {
      await apiPost(path)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
    } finally {
      setBusy(false)
    }
  }

  async function remove(deleteFiles: boolean) {
    setBusy(true)
    try {
      await apiDelete(`/torrents/${t.hash}?delete_files=${deleteFiles}`)
      qc.invalidateQueries({ queryKey: ['torrents'], exact: false })
      onClose()
    } finally {
      setBusy(false)
    }
  }

  const progress = t.size_bytes > 0 ? (t.bytes_done / t.size_bytes) * 100 : 0
  const ratio = (t.ratio / 1000).toFixed(3)
  const tags = t.tags ? t.tags.split(',').filter(Boolean) : []
  const isRunning = t.is_open && t.is_active

  return (
    <aside style={{
      width: 340, background: '#0d1117', borderLeft: '1px solid #1e2433',
      display: 'flex', flexDirection: 'column', flexShrink: 0, fontSize: 12,
    }}>
      {/* Header */}
      <div style={{
        padding: '10px 14px', borderBottom: '1px solid #1e2433',
        display: 'flex', alignItems: 'flex-start', gap: 8,
      }}>
        <span style={{ flex: 1, fontWeight: 600, fontSize: 13, color: '#e2e8f0', lineHeight: 1.3 }}>
          {t.name}
        </span>
        <button onClick={onClose} style={{
          background: 'none', border: 'none', cursor: 'pointer',
          color: '#475569', fontSize: 16, padding: '0 4px', flexShrink: 0,
        }}>✕</button>
      </div>

      {/* Action buttons */}
      <div style={{
        padding: '8px 14px', borderBottom: '1px solid #1e2433',
        display: 'flex', flexWrap: 'wrap', gap: 6,
      }}>
        {isRunning
          ? <ActionBtn label="Stop"      color="#64748b" disabled={busy} onClick={() => action(`/torrents/${t.hash}/stop`)} />
          : <ActionBtn label="Start"     color="#22c55e" disabled={busy} onClick={() => action(`/torrents/${t.hash}/start`)} />
        }
        <ActionBtn label="Recheck"    color="#f59e0b" disabled={busy} onClick={() => action(`/torrents/${t.hash}/recheck`)} />
        <ActionBtn label="Reannounce" color="#3b82f6" disabled={busy} onClick={() => action(`/torrents/${t.hash}/reannounce`)} />
        <ActionBtn label="Delete"     color="#ef4444" disabled={busy} onClick={() => setConfirmDelete(true)} />
      </div>

      {/* Delete confirmation */}
      {confirmDelete && (
        <div style={{
          padding: '10px 14px', background: '#1a0a0a', borderBottom: '1px solid #7f1d1d',
          fontSize: 12,
        }}>
          <div style={{ color: '#fca5a5', marginBottom: 8 }}>Delete "{t.name}"?</div>
          <div style={{ display: 'flex', gap: 6 }}>
            <button onClick={() => remove(false)} style={{
              background: '#7f1d1d', border: 'none', borderRadius: 4,
              color: '#fca5a5', padding: '3px 10px', fontSize: 11, cursor: 'pointer',
            }}>Remove torrent</button>
            <button onClick={() => remove(true)} style={{
              background: '#991b1b', border: 'none', borderRadius: 4,
              color: '#fca5a5', padding: '3px 10px', fontSize: 11, cursor: 'pointer',
            }}>+ Delete files</button>
            <button onClick={() => setConfirmDelete(false)} style={{
              background: 'none', border: '1px solid #334155', borderRadius: 4,
              color: '#64748b', padding: '3px 8px', fontSize: 11, cursor: 'pointer',
            }}>Cancel</button>
          </div>
        </div>
      )}

      {/* Scrollable body */}
      <div style={{ flex: 1, overflowY: 'auto', padding: '14px 14px 0' }}>

        {/* Progress bar */}
        <div style={{ marginBottom: 14 }}>
          <div style={{ display: 'flex', justifyContent: 'space-between', marginBottom: 4 }}>
            <span style={{ fontSize: 11, color: '#64748b' }}>Progress</span>
            <span style={{ fontSize: 11, color: '#94a3b8' }}>{progress.toFixed(1)}%</span>
          </div>
          <div style={{ height: 4, background: '#1e2433', borderRadius: 2, overflow: 'hidden' }}>
            <div style={{
              width: `${progress}%`, height: '100%',
              background: progress >= 100 ? '#22c55e' : '#3b82f6', borderRadius: 2,
            }} />
          </div>
        </div>

        {/* Stats grid */}
        <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr', gap: '0 16px' }}>
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
            <div key={lbl}>
              <div style={LABEL}>{lbl}</div>
              <div style={VALUE}>{val}</div>
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
                background: '#1e2433', color: '#94a3b8',
                padding: '1px 7px', borderRadius: 10, fontSize: 11,
              }}>{tag}</span>
            ))}
          </div>
        </>)}

        <div style={LABEL}>Save path</div>
        <div style={{ ...MONO, marginBottom: 12 }}>{t.directory || '—'}</div>

        {t.message && (<>
          <div style={LABEL}>Message</div>
          <div style={{ ...VALUE, color: '#f59e0b' }}>{t.message}</div>
        </>)}

        <div style={LABEL}>Hash</div>
        <div style={MONO}>{t.hash}</div>

        {/* Trackers */}
        {trackers && trackers.length > 0 && (
          <Section title="Trackers">
            {trackers.map((tr, i) => (
              <div key={i} style={{ marginBottom: 8 }}>
                <div style={{
                  fontSize: 11, color: '#94a3b8', overflow: 'hidden',
                  textOverflow: 'ellipsis', whiteSpace: 'nowrap', fontFamily: 'monospace',
                }} title={tr.url}>{tr.url}</div>
                <div style={{ fontSize: 10, color: '#475569', marginTop: 1 }}>
                  {tr.scrape_complete} seeds · {tr.scrape_incomplete} peers
                  {tr.message ? ` · ${tr.message}` : ''}
                </div>
              </div>
            ))}
          </Section>
        )}

        {/* Files */}
        {files && files.length > 0 && (
          <Section title={`Files (${files.length})`}>
            {files.map(f => {
              const fp = f.size_chunks > 0 ? (f.completed_chunks / f.size_chunks) * 100 : 100
              return (
                <div key={f.index} style={{ marginBottom: 8 }}>
                  <div style={{
                    fontSize: 11, color: '#94a3b8', overflow: 'hidden',
                    textOverflow: 'ellipsis', whiteSpace: 'nowrap',
                  }} title={f.path}>{f.path.split('/').pop()}</div>
                  <div style={{ display: 'flex', gap: 8, alignItems: 'center', marginTop: 2 }}>
                    <div style={{ flex: 1, height: 2, background: '#1e2433', borderRadius: 1, overflow: 'hidden' }}>
                      <div style={{ width: `${fp}%`, height: '100%', background: fp >= 100 ? '#22c55e' : '#3b82f6' }} />
                    </div>
                    <span style={{ fontSize: 10, color: '#475569', flexShrink: 0 }}>{fmtSize(f.size_bytes)}</span>
                  </div>
                </div>
              )
            })}
          </Section>
        )}

        <div style={{ height: 14 }} />
      </div>
    </aside>
  )
}

function Section({ title, children }: { title: string; children: React.ReactNode }) {
  return (
    <div style={{ marginTop: 16 }}>
      <div style={{
        fontSize: 10, fontWeight: 600, letterSpacing: '0.06em', textTransform: 'uppercase',
        color: '#3b82f6', borderBottom: '1px solid #1e2433', paddingBottom: 4, marginBottom: 8,
      }}>{title}</div>
      {children}
    </div>
  )
}
