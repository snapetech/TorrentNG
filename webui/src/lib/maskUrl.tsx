import { useState } from 'react'

/**
 * Shows a tracker origin without any URL components that may contain a
 * credential. The TrackerUrl control has an explicit reveal/copy path for
 * users who need the complete announce URL.
 */
export function maskAnnounceUrl(url: string): string {
  return redactUrlOrigin(url, ['http:', 'https:', 'udp:'])
}

/** Hides every URL component except the HTTP(S) origin for configuration labels. */
export function redactUrlForDisplay(url: string): string {
  return redactUrlOrigin(url, ['http:', 'https:'])
}

function redactUrlOrigin(url: string, allowedProtocols: readonly string[]): string {
  if (!url) return url

  try {
    const parsed = new URL(url)
    if (!allowedProtocols.includes(parsed.protocol) || !parsed.host) return '[redacted URL]'
    return `${parsed.protocol}//${parsed.host}/…`
  } catch {
    return '[redacted URL]'
  }
}

const BTN: React.CSSProperties = {
  background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
  color: 'var(--faint)', padding: '1px 6px', fontSize: 10, cursor: 'pointer', flexShrink: 0,
}

/**
 * Displays a tracker announce URL masked by default (see maskAnnounceUrl),
 * with a click-to-reveal toggle and a copy button that always copies the
 * real, unmasked URL regardless of reveal state.
 */
export function TrackerUrl({ url, mono = true }: { url: string; mono?: boolean }) {
  const [revealed, setRevealed] = useState(false)
  const [copied, setCopied] = useState(false)
  if (!url) return <span>-</span>

  async function copy() {
    try {
      if (!navigator.clipboard) return
      await navigator.clipboard.writeText(url)
      setCopied(true)
      window.setTimeout(() => setCopied(false), 1500)
    } catch {
      // Clipboard access can be denied by the browser; nothing else to do.
    }
  }

  return (
    <span style={{ display: 'inline-flex', alignItems: 'center', gap: 6, minWidth: 0 }}>
      <span
        style={{
          fontFamily: mono ? 'monospace' : undefined,
          overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap', minWidth: 0,
        }}
        title={revealed ? url : 'Passkey hidden - click Show to reveal'}
      >
        {revealed ? url : maskAnnounceUrl(url)}
      </span>
      <button type="button" onClick={() => setRevealed(v => !v)} style={BTN} title={revealed ? 'Hide credential' : 'Reveal credential'}>
        {revealed ? 'Hide' : 'Show'}
      </button>
      <button type="button" onClick={copy} style={BTN} title="Copy the real tracker URL">
        {copied ? 'Copied' : 'Copy'}
      </button>
    </span>
  )
}
