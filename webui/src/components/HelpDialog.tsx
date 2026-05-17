interface Props {
  onClose: () => void
}

const SHORTCUTS = [
  ['A', 'Open add torrent dialog'],
  ['Esc', 'Close dialog, details, or clear selection'],
  ['Click row', 'Select or deselect torrent'],
  ['Double click row', 'Open details'],
  ['Right click row', 'Select and open actions'],
  ['Column header', 'Sort table'],
]

const LINKS = [
  ['Project', 'https://github.com/snapetech/rtorrentNG'],
  ['Discord support', 'https://discord.gg/4ub88HeHFm'],
  ['qBittorrent API compatibility', 'https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-4.1)'],
]

export function HelpDialog({ onClose }: Props) {
  return (
    <div className="rtng-modal-backdrop" style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1100,
      display: 'grid', placeItems: 'center', padding: 24,
    }} onClick={e => { if (e.target === e.currentTarget) onClose() }}>
      <div role="dialog" aria-modal="true" aria-label="Help" className="rtng-modal" style={{
        width: 'min(620px, 100%)', maxHeight: '80vh', overflowY: 'auto',
        background: 'var(--panel)', border: '1px solid var(--border-strong)', borderRadius: 8,
        boxShadow: '0 24px 60px var(--shadow)',
      }} onClick={e => e.stopPropagation()}>
        <div style={{
          display: 'flex', alignItems: 'center', gap: 12, padding: '14px 16px',
          borderBottom: '1px solid var(--border)',
        }}>
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 16, fontWeight: 700, color: 'var(--text)' }}>Help</div>
            <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 2 }}>rtorrentNG WebUI controls and support links</div>
          </div>
          <button onClick={onClose} style={closeButton}>Close</button>
        </div>

        <div style={{ padding: 16, display: 'grid', gap: 18 }}>
          <section className="rtng-card" style={sectionCard}>
            <h2 style={headingStyle}>Shortcuts</h2>
            <div style={{ display: 'grid', gap: 6 }}>
              {SHORTCUTS.map(([key, value]) => (
                <div key={key} style={rowStyle}>
                  <kbd style={kbdStyle}>{key}</kbd>
                  <span>{value}</span>
                </div>
              ))}
            </div>
          </section>

          <section className="rtng-card" style={sectionCard}>
            <h2 style={headingStyle}>Actions</h2>
            <p style={textStyle}>
              Use the toolbar for selected torrents, the left sidebar for filtering and saved views,
              and the details panel for trackers, files, save path, hash, and destructive actions.
            </p>
          </section>

          <section className="rtng-card" style={sectionCard}>
            <h2 style={headingStyle}>Links</h2>
            <div style={{ display: 'grid', gap: 7 }}>
              {LINKS.map(([label, href]) => (
                <a key={href} className="rtng-card-link" href={href} target="_blank" rel="noreferrer" style={linkStyle}>
                  <span>{label}</span>
                  <span style={{ color: 'var(--faint)', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{href.replace(/^https?:\/\//, '')}</span>
                  <span aria-hidden="true" style={{ color: 'var(--accent-text)', justifySelf: 'end' }}>↗</span>
                </a>
              ))}
            </div>
          </section>
        </div>
      </div>
    </div>
  )
}

const headingStyle: React.CSSProperties = {
  margin: '0 0 8px',
  color: 'var(--accent)',
  fontSize: 12,
  textTransform: 'uppercase',
  letterSpacing: '0.06em',
}

const sectionCard: React.CSSProperties = {
  border: '1px solid var(--border)',
  borderRadius: 8,
  background: 'color-mix(in srgb, var(--surface) 84%, var(--bg))',
  padding: 12,
  boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.03)',
}

const rowStyle: React.CSSProperties = {
  display: 'grid',
  gridTemplateColumns: '90px 1fr',
  gap: 10,
  alignItems: 'center',
  color: 'var(--text)',
  fontSize: 13,
}

const kbdStyle: React.CSSProperties = {
  display: 'inline-block',
  width: 'fit-content',
  minWidth: 34,
  padding: '2px 7px',
  border: '1px solid var(--border-strong)',
  borderRadius: 4,
  background: 'var(--bg)',
  color: 'var(--text)',
  fontFamily: 'monospace',
  fontSize: 12,
}

const textStyle: React.CSSProperties = {
  margin: 0,
  color: 'var(--text)',
  fontSize: 13,
  lineHeight: 1.5,
}

const linkStyle: React.CSSProperties = {
  color: 'var(--accent)',
  fontSize: 13,
  textDecoration: 'none',
  display: 'grid',
  gridTemplateColumns: '150px minmax(0, 1fr) auto',
  gap: 10,
  alignItems: 'center',
  border: '1px solid var(--border)',
  borderRadius: 6,
  background: 'var(--surface)',
  padding: '7px 9px',
}

const closeButton: React.CSSProperties = {
  background: 'transparent',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--muted)',
  padding: '5px 10px',
  fontSize: 12,
  cursor: 'pointer',
}
