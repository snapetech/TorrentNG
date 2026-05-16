import type { TorrentSummary } from '../api/client'

export interface ContextMenuState {
  x: number
  y: number
  torrent: TorrentSummary
}

interface Props {
  menu: ContextMenuState
  onClose: () => void
  onProperties: () => void
  onEditSelected: () => void
  onDetail: () => void
  onStart: () => void
  onStop: () => void
  onRecheck: () => void
  onReannounce: () => void
  onDelete: () => void
  onCopyHash: () => void
  onCopyName: () => void
  onToggleSequential: () => void
}

export function TorrentContextMenu({
  menu, onClose, onProperties, onEditSelected, onDetail, onStart, onStop, onRecheck, onReannounce, onDelete,
  onCopyHash, onCopyName, onToggleSequential,
}: Props) {
  const isRunning = menu.torrent.is_open && menu.torrent.is_active
  const items = [
    { label: 'Properties...', action: onProperties },
    { label: 'Edit selected...', action: onEditSelected },
    { label: 'Show details', action: onDetail },
    isRunning ? { label: 'Stop', action: onStop } : { label: 'Start', action: onStart },
    { label: 'Recheck', action: onRecheck },
    { label: 'Reannounce', action: onReannounce },
    { label: 'Toggle sequential download', action: onToggleSequential },
    { label: 'Copy hash', action: onCopyHash },
    { label: 'Copy name', action: onCopyName },
    { label: 'Delete...', action: onDelete, danger: true },
  ]

  function run(action: () => void) {
    action()
    onClose()
  }

  return (
    <div
      onMouseLeave={onClose}
      style={{
        position: 'fixed', left: menu.x, top: menu.y, zIndex: 1000,
        minWidth: 176, background: '#0f141d', border: '1px solid #334155',
        borderRadius: 6, boxShadow: '0 18px 40px rgba(0,0,0,0.45)', padding: 4,
      }}
    >
      <div style={{
        padding: '6px 8px', borderBottom: '1px solid #1e2433', color: '#64748b',
        fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap',
      }}>
        {menu.torrent.name}
      </div>
      {items.map(item => (
        <button
          key={item.label}
          onClick={() => run(item.action)}
          style={{
            width: '100%', display: 'block', textAlign: 'left', background: 'transparent',
            border: 'none', borderRadius: 4, color: item.danger ? '#f87171' : '#cbd5e1',
            padding: '6px 8px', fontSize: 12, cursor: 'pointer',
          }}
        >
          {item.label}
        </button>
      ))}
    </div>
  )
}
