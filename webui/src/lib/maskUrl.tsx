import { useState } from 'react'

const CREDENTIAL_QUERY_KEYS = new Set([
  'passkey', 'pass', 'pid', 'auth', 'authkey', 'token', 'key', 'secret', 'uk', 'rss_key', 'apikey',
])

const MASK = '•'.repeat(8)

function decodedLower(value: string): string {
  try {
    return decodeURIComponent(value).toLowerCase()
  } catch {
    // A malformed percent escape must not make a render fail. It also cannot
    // safely be treated as a credential key, so compare the raw spelling.
    return value.toLowerCase()
  }
}

/**
 * Masks credential-shaped parts of a tracker announce URL so it can be shown
 * on screen (and screen-shared/screenshotted) without leaking a private
 * tracker passkey. Two shapes are handled: known credential query params
 * (?passkey=...) and long opaque path segments some trackers use instead
 * (e.g. myanonamouse's /tracker.php/<passkey>/announce).
 *
 * The host is always left visible - masking exists to hide the credential,
 * not to hide which tracker this is.
 */
export function maskAnnounceUrl(url: string): string {
  if (!url) return url

  // Work on the raw string (not URL/URLSearchParams.toString(), which would
  // percent-encode the bullet placeholder into "%E2%80%A2..." garbage) so
  // the masked query value renders as readable text.
  let masked = url.replace(
    /^([a-z][a-z\d+.-]*:\/\/)([^/?#@]+)@/i,
    `$1${MASK}@`,
  ).replace(
    /([?&])([^=&#]+)=([^&#]*)/g,
    (match, sep: string, key: string, value: string) =>
      CREDENTIAL_QUERY_KEYS.has(decodedLower(key)) && value
        ? `${sep}${key}=${MASK}`
        : match,
  )

  const hashIndex = masked.indexOf('#')
  const pathAndQuery = hashIndex === -1 ? masked : masked.slice(0, hashIndex)
  const fragment = hashIndex === -1 ? '' : masked.slice(hashIndex)
  const queryIndex = pathAndQuery.indexOf('?')
  const pathPart = queryIndex === -1 ? pathAndQuery : pathAndQuery.slice(0, queryIndex)
  const queryPart = queryIndex === -1 ? '' : pathAndQuery.slice(queryIndex)
  masked = maskOpaquePathSegments(pathPart) + queryPart + fragment

  return masked
}

/** A path segment of 16+ alphanumeric characters is almost certainly an opaque token, not a real path. */
function maskOpaquePathSegments(pathOrUrl: string): string {
  return pathOrUrl.replace(/([/])([A-Za-z0-9]{16,})(?=[/?#]|$)/g, (_match, sep: string, segment: string) => {
    const looksOpaque = /[0-9]/.test(segment) && /[A-Za-z]/.test(segment)
    if (!looksOpaque) return `${sep}${segment}`
    return `${sep}${'•'.repeat(8)}`
  })
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
      await navigator.clipboard?.writeText(url)
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
