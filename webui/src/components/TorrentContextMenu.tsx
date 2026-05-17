import { useEffect, useRef } from 'react'
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
  const menuRef = useRef<HTMLDivElement>(null)
  const isRunning = menu.torrent.is_open && menu.torrent.is_active
  const left = Math.min(menu.x, window.innerWidth - 236)
  const top = Math.min(menu.y, window.innerHeight - 356)
  const status = menu.torrent.message && !menu.torrent.is_active
    ? { label: 'Error', color: 'var(--danger)' }
    : !menu.torrent.is_open
      ? { label: 'Stopped', color: 'var(--faint)' }
      : menu.torrent.complete && menu.torrent.is_active
        ? { label: 'Seeding', color: 'var(--success)' }
        : menu.torrent.is_active
          ? { label: 'Downloading', color: 'var(--accent)' }
          : { label: 'Queued', color: 'var(--muted)' }
  const items = [
    { label: 'Properties...', icon: '⌘', action: onProperties },
    { label: 'Edit selected...', icon: '✎', action: onEditSelected },
    { label: 'Show details', icon: '▣', action: onDetail },
    { separator: true, label: 'run-separator', action: () => undefined },
    isRunning ? { label: 'Stop', icon: '■', action: onStop } : { label: 'Start', icon: '▶', action: onStart },
    { label: 'Recheck', icon: '↻', action: onRecheck },
    { label: 'Reannounce', icon: '⇄', action: onReannounce },
    { label: 'Toggle sequential download', icon: '≡', action: onToggleSequential },
    { separator: true, label: 'copy-separator', action: () => undefined },
    { label: 'Copy hash', icon: '#', action: onCopyHash },
    { label: 'Copy name', icon: 'T', action: onCopyName },
    { separator: true, label: 'delete-separator', action: () => undefined },
    { label: 'Delete...', icon: '!', action: onDelete, danger: true },
  ]

  function run(action: () => void) {
    action()
    onClose()
  }

  useEffect(() => {
    function onPointerDown(e: PointerEvent) {
      if (menuRef.current?.contains(e.target as Node)) return
      onClose()
    }
    function onKeyDown(e: KeyboardEvent) {
      if (e.key === 'Escape') onClose()
    }
    window.addEventListener('pointerdown', onPointerDown)
    window.addEventListener('keydown', onKeyDown)
    return () => {
      window.removeEventListener('pointerdown', onPointerDown)
      window.removeEventListener('keydown', onKeyDown)
    }
  }, [onClose])

  return (
    <div
      ref={menuRef}
      className="tng-context-menu"
      data-status={status.label.toLowerCase()}
      role="menu"
      aria-label={`Actions for ${menu.torrent.name}`}
      onContextMenu={e => e.preventDefault()}
      style={{
        position: 'fixed', left: Math.max(8, left), top: Math.max(8, top), zIndex: 1000,
        width: 228, background: 'var(--panel)', border: '1px solid var(--border-strong)',
        borderRadius: 6, boxShadow: '0 18px 40px var(--shadow)', padding: 4,
      }}
    >
      <div style={{
        padding: '7px 8px', borderBottom: '1px solid var(--border)', color: 'var(--faint)',
        fontSize: 11, display: 'grid', gap: 5,
      }}>
        <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{menu.torrent.name}</span>
        <span style={{ display: 'flex', alignItems: 'center', gap: 6 }}>
          <span style={{
            width: 7, height: 7, borderRadius: 999, background: status.color,
            boxShadow: `0 0 12px color-mix(in srgb, ${status.color} 45%, transparent)`,
          }} />
          <span style={{ color: status.color, fontWeight: 700 }}>{status.label}</span>
          <span style={{ marginLeft: 'auto' }}>{(menu.torrent.ratio / 1000).toFixed(2)} ratio</span>
        </span>
      </div>
      {items.map(item => item.separator ? (
        <div key={item.label} className="tng-context-separator" style={{ height: 1, background: 'var(--border)', margin: '4px 5px' }} />
      ) : (
        <button
          key={item.label}
          className="tng-context-item"
          data-danger={item.danger ? 'true' : 'false'}
          role="menuitem"
          tabIndex={0}
          onClick={() => run(item.action)}
          style={{
            width: '100%', display: 'grid', gridTemplateColumns: '22px 1fr', alignItems: 'center',
            textAlign: 'left', background: 'transparent',
            border: 'none', borderRadius: 4, color: item.danger ? 'var(--danger)' : 'var(--text)',
            padding: '6px 8px', fontSize: 12, cursor: 'pointer',
          }}
        >
          <span style={{ color: item.danger ? 'var(--danger)' : 'var(--faint)', fontSize: 11 }}>{item.icon}</span>
          <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{item.label}</span>
        </button>
      ))}
    </div>
  )
}
