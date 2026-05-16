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
  onHelp: () => void
  busy: boolean
}

const ACTIONS = [
  { key: 'start', label: 'Start', title: 'Start selected torrents', color: '#22c55e' },
  { key: 'stop', label: 'Stop', title: 'Stop selected torrents', color: '#94a3b8' },
  { key: 'recheck', label: 'Recheck', title: 'Force hash check', color: '#f59e0b' },
  { key: 'reannounce', label: 'Announce', title: 'Reannounce to trackers', color: '#3b82f6' },
]

export function TorrentToolbar({
  selectedCount, onAdd, onStart, onStop, onRecheck, onReannounce, onProperties, onEditSelected, onSequential, onHelp, busy,
}: Props) {
  const disabled = selectedCount === 0 || busy
  const handlers: Record<string, () => void> = {
    start: onStart,
    stop: onStop,
    recheck: onRecheck,
    reannounce: onReannounce,
  }

  return (
    <div style={{
      height: 38, flexShrink: 0, display: 'flex', alignItems: 'center', gap: 6,
      padding: '0 10px', background: 'var(--surface)', borderBottom: '1px solid var(--border)',
    }}>
      <button onClick={onAdd} title="Add torrent" style={primaryButton}>+ Add</button>
      <div style={{ width: 1, height: 20, background: 'var(--border)', margin: '0 2px' }} />
      {ACTIONS.map(action => (
        <button
          key={action.key}
          onClick={handlers[action.key]}
          disabled={disabled}
          title={action.title}
          style={actionButton(action.color, disabled)}
        >
          {action.label}
        </button>
      ))}
      <button
        onClick={onProperties}
        disabled={selectedCount !== 1 || busy}
        title="Open selected torrent properties"
        style={actionButton('#93c5fd', selectedCount !== 1 || busy)}
      >
        Properties
      </button>
      <button
        onClick={onEditSelected}
        disabled={disabled}
        title="Bulk edit selected torrents"
        style={actionButton('#93c5fd', disabled)}
      >
        Edit selected
      </button>
      <button
        onClick={onSequential}
        disabled={disabled}
        title="Toggle sequential download for selected torrents"
        style={actionButton('#f59e0b', disabled)}
      >
        Sequential
      </button>
      <span style={{ color: 'var(--faint)', fontSize: 11, marginLeft: 6 }}>
        {selectedCount > 0 ? `${selectedCount.toLocaleString()} selected` : 'No selection'}
      </span>
      <button onClick={onHelp} title="Keyboard shortcuts and docs" style={{
        marginLeft: 'auto', background: 'transparent', border: '1px solid var(--border-strong)',
        borderRadius: 5, color: 'var(--muted)', padding: '4px 8px', fontSize: 12, cursor: 'pointer',
      }}>
        Help
      </button>
    </div>
  )
}

const primaryButton: React.CSSProperties = {
  background: 'var(--accent-soft)',
  border: '1px solid var(--accent)',
  borderRadius: 5,
  color: 'var(--accent-text)',
  padding: '4px 10px',
  fontSize: 12,
  cursor: 'pointer',
}

function actionButton(color: string, disabled: boolean): React.CSSProperties {
  return {
    background: 'var(--surface-2)',
    border: `1px solid ${color}55`,
    borderRadius: 5,
    color,
    padding: '4px 9px',
    fontSize: 12,
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.45 : 1,
  }
}
