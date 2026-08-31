interface Props {
  selectedCount: number
  onAdd: () => void
  onStart: () => void
  onStop: () => void
  onRecheck: () => void
  onReannounce: () => void
  onProperties: () => void
  onEditSelected: () => void
  onSequential: () => void
  onClearSelection: () => void
  onHelp: () => void
  busy: boolean
}

const ACTIONS = [
  { key: 'start', icon: '▶', label: 'Start', title: 'Start selected torrents', color: 'var(--success)' },
  { key: 'stop', icon: '■', label: 'Stop', title: 'Stop selected torrents', color: 'var(--muted)' },
  { key: 'recheck', icon: '↻', label: 'Recheck', title: 'Force hash check', color: 'var(--warning)' },
  { key: 'reannounce', icon: '⇄', label: 'Announce', title: 'Reannounce to trackers', color: 'var(--accent)' },
]

export function TorrentToolbar({
  selectedCount, onAdd, onStart, onStop, onRecheck, onReannounce, onProperties, onEditSelected, onSequential,
  onClearSelection, onHelp, busy,
}: Props) {
  const disabled = selectedCount === 0 || busy
  const handlers: Record<string, () => void> = {
    start: onStart,
    stop: onStop,
    recheck: onRecheck,
    reannounce: onReannounce,
  }

  return (
    <div className="tng-toolbar" data-has-selection={selectedCount > 0 ? 'true' : 'false'} data-busy={busy ? 'true' : 'false'} style={{
      minHeight: 40, flexShrink: 0, display: 'flex', alignItems: 'center', gap: 8,
      padding: '0 10px', background: 'var(--surface)', borderBottom: '1px solid var(--border)',
      minWidth: 0, overflowX: 'auto', overflowY: 'hidden', scrollbarWidth: 'thin',
    }}>
      <button className="tng-toolbar-button" onClick={onAdd} title="Add torrent" aria-label="Add torrent" style={primaryButton}><span>+</span><span>Add</span></button>
      <div className="tng-toolbar-divider" style={{ width: 1, height: 20, background: 'var(--border)', margin: '0 2px' }} />
      <span className="tng-toolbar-label" style={groupLabel}>Transfer</span>
      <div className="tng-toolbar-group" style={buttonGroup}>
        {ACTIONS.map(action => (
          <button
            className="tng-toolbar-button"
            key={action.key}
            onClick={handlers[action.key]}
            disabled={disabled}
            title={action.title}
            aria-label={action.title}
            style={actionButton(action.color, disabled)}
        >
            <span>{action.icon}</span><span>{action.label}</span>
          </button>
        ))}
      </div>
      <span className="tng-toolbar-label" style={groupLabel}>Edit</span>
      <div className="tng-toolbar-group" style={buttonGroup}>
        <button
          className="tng-toolbar-button"
          onClick={onProperties}
          disabled={selectedCount !== 1 || busy}
          title="Open selected torrent properties"
          aria-label="Open selected torrent properties"
          style={actionButton('var(--accent)', selectedCount !== 1 || busy)}
        >
          <span>⌘</span><span>Properties</span>
        </button>
        <button
          className="tng-toolbar-button"
          onClick={onEditSelected}
          disabled={disabled}
          title="Bulk edit selected torrents"
          aria-label="Bulk edit selected torrents"
          style={actionButton('var(--accent)', disabled)}
        >
          <span>✎</span><span>Edit selected</span>
        </button>
        <button
          className="tng-toolbar-button"
          onClick={onSequential}
          disabled={disabled}
          title="Toggle sequential download for selected torrents"
          aria-label="Toggle sequential download for selected torrents"
          style={actionButton('var(--warning)', disabled)}
        >
          <span>≡</span><span>Sequential</span>
        </button>
      </div>
      <span className="tng-toolbar-selection" style={{
        color: selectedCount > 0 ? 'var(--accent-text)' : 'var(--faint)',
        fontSize: 11, marginLeft: 2, whiteSpace: 'nowrap', flex: '0 0 auto',
        padding: '3px 7px', border: '1px solid var(--border)', borderRadius: 5,
        background: selectedCount > 0 ? 'var(--accent-soft)' : 'transparent',
        fontWeight: selectedCount > 0 ? 800 : 600,
      }}>
        {busy ? 'Working...' : selectedCount > 0 ? `${selectedCount.toLocaleString()} selected` : 'No selection'}
      </span>
      {selectedCount > 0 && (
        <button
          className="tng-toolbar-button"
          onClick={onClearSelection}
          disabled={busy}
          title="Clear selected torrents"
          aria-label="Clear selected torrents"
          style={actionButton('var(--muted)', busy)}
        >
          <span>×</span><span>Clear</span>
        </button>
      )}
      <button className="tng-toolbar-help tng-toolbar-button" onClick={onHelp} title="Keyboard shortcuts and docs" aria-label="Keyboard shortcuts and docs" style={{
        marginLeft: 'auto', flex: '0 0 auto', background: 'transparent', border: '1px solid var(--border-strong)',
        borderRadius: 5, color: 'var(--muted)', padding: '4px 8px', fontSize: 12, cursor: 'pointer',
      }}>
        <span>?</span><span>Help</span>
      </button>
    </div>
  )
}

const primaryButton: React.CSSProperties = {
  flex: '0 0 auto',
  display: 'inline-flex',
  alignItems: 'center',
  gap: 5,
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent)',
  borderRadius: 5,
  color: 'var(--accent-text)',
  padding: '4px 10px',
  fontSize: 12,
  cursor: 'pointer',
}

const buttonGroup: React.CSSProperties = {
  flex: '0 0 auto',
  display: 'inline-flex',
  alignItems: 'center',
  gap: 5,
}

const groupLabel: React.CSSProperties = {
  flex: '0 0 auto',
  color: 'var(--faint)',
  fontSize: 10,
  fontWeight: 800,
  textTransform: 'uppercase',
  letterSpacing: 0,
  marginLeft: 2,
}

function actionButton(color: string, disabled: boolean): React.CSSProperties {
  return {
    background: 'var(--surface-2)',
    flex: '0 0 auto',
    display: 'inline-flex',
    alignItems: 'center',
    gap: 5,
    border: `1px solid color-mix(in srgb, ${color} 42%, var(--border))`,
    borderRadius: 5,
    // Text needs AA contrast (4.5:1) against the button background; the raw
    // tint color alone can fall short (e.g. the default theme's accent blue
    // measured 4.39:1 on --surface-2), so blend it toward --text, which is
    // tuned for that contrast, while keeping a visible per-action tint.
    color: `color-mix(in srgb, ${color} 82%, var(--text))`,
    padding: '4px 9px',
    fontSize: 12,
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.45 : 1,
  }
}
