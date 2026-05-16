interface Props {
  onClose: () => void
}

const SHORTCUTS = [
  ['A', 'Open add torrent dialog'],
  ['Esc', 'Close dialog, details, or clear selection'],
  ['Click row name', 'Open or close details'],
  ['Right click row', 'Open torrent actions'],
  ['Column header', 'Sort table'],
]

const LINKS = [
  ['Project', 'https://github.com/rtorrentng/rtorrentng'],
  ['Discord support', 'https://discord.gg/4ub88HeHFm'],
  ['qBittorrent API compatibility', 'https://github.com/qbittorrent/qBittorrent/wiki/WebUI-API-(qBittorrent-4.1)'],
]

export function HelpDialog({ onClose }: Props) {
  return (
    <div style={{
      position: 'fixed', inset: 0, background: 'rgba(2,6,23,0.72)', zIndex: 1100,
      display: 'grid', placeItems: 'center', padding: 24,
    }}>
      <div style={{
        width: 'min(620px, 100%)', maxHeight: '80vh', overflowY: 'auto',
        background: '#0f141d', border: '1px solid #334155', borderRadius: 8,
        boxShadow: '0 24px 60px rgba(0,0,0,0.5)',
      }}>
        <div style={{
          display: 'flex', alignItems: 'center', gap: 12, padding: '14px 16px',
          borderBottom: '1px solid #1e2433',
        }}>
          <div style={{ flex: 1 }}>
            <div style={{ fontSize: 16, fontWeight: 700, color: '#e2e8f0' }}>Help</div>
            <div style={{ fontSize: 12, color: '#64748b', marginTop: 2 }}>rtorrentNG WebUI controls and support links</div>
          </div>
          <button onClick={onClose} style={closeButton}>Close</button>
        </div>

        <div style={{ padding: 16, display: 'grid', gap: 18 }}>
          <section>
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

          <section>
            <h2 style={headingStyle}>Actions</h2>
            <p style={textStyle}>
              Use the toolbar for selected torrents, the left sidebar for filtering and saved views,
              and the details panel for trackers, files, save path, hash, and destructive actions.
            </p>
          </section>

          <section>
            <h2 style={headingStyle}>Links</h2>
            <div style={{ display: 'grid', gap: 7 }}>
              {LINKS.map(([label, href]) => (
                <a key={href} href={href} target="_blank" rel="noreferrer" style={linkStyle}>{label}</a>
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
  color: '#93c5fd',
  fontSize: 12,
  textTransform: 'uppercase',
  letterSpacing: '0.06em',
}

const rowStyle: React.CSSProperties = {
  display: 'grid',
  gridTemplateColumns: '90px 1fr',
  gap: 10,
  alignItems: 'center',
  color: '#cbd5e1',
  fontSize: 13,
}

const kbdStyle: React.CSSProperties = {
  display: 'inline-block',
  width: 'fit-content',
  minWidth: 34,
  padding: '2px 7px',
  border: '1px solid #334155',
  borderRadius: 4,
  background: '#0d1117',
  color: '#e2e8f0',
  fontFamily: 'monospace',
  fontSize: 12,
}

const textStyle: React.CSSProperties = {
  margin: 0,
  color: '#cbd5e1',
  fontSize: 13,
  lineHeight: 1.5,
}

const linkStyle: React.CSSProperties = {
  color: '#93c5fd',
  fontSize: 13,
  textDecoration: 'none',
}

const closeButton: React.CSSProperties = {
  background: 'transparent',
  border: '1px solid #334155',
  borderRadius: 5,
  color: '#94a3b8',
  padding: '5px 10px',
  fontSize: 12,
  cursor: 'pointer',
}
