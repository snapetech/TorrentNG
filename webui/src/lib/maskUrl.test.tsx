import { describe, expect, it } from 'vitest'
import { maskAnnounceUrl, redactUrlForDisplay } from './maskUrl'

describe('maskAnnounceUrl', () => {
  it('shows only the HTTP tracker origin despite userinfo, path, query, and fragment secrets', () => {
    const masked = maskAnnounceUrl(
      'https://user:auth-secret@tracker.example/short-passkey/announce?signature=query-secret#fragment-secret',
    )

    expect(masked).toBe('https://tracker.example/…')
    for (const secret of ['user', 'auth-secret', 'short-passkey', 'query-secret', 'fragment-secret']) {
      expect(masked).not.toContain(secret)
    }
  })

  it('shows UDP tracker origins and fails closed for unsupported or malformed URLs', () => {
    expect(maskAnnounceUrl('udp://user:password@tracker.example:6969/key/announce?sig=secret'))
      .toBe('udp://tracker.example:6969/…')
    expect(maskAnnounceUrl('magnet:?xt=urn:btih:secret')).toBe('[redacted URL]')
    expect(maskAnnounceUrl('not a URL with passkey=secret')).toBe('[redacted URL]')
  })

  it('redacts all endpoint details while retaining a valid HTTP(S) origin', () => {
    const redacted = redactUrlForDisplay(
      'https://user:auth-secret@hooks.example/api/webhooks/123/opaque-token?signature=query-secret#fragment-secret',
    )

    expect(redacted).toBe('https://hooks.example/…')
    for (const secret of ['user', 'auth-secret', 'opaque-token', 'query-secret', 'fragment-secret']) {
      expect(redacted).not.toContain(secret)
    }
  })

  it('fails closed for malformed and non-HTTP(S) endpoint strings', () => {
    expect(redactUrlForDisplay('not a URL with credential=secret')).toBe('[redacted URL]')
    expect(redactUrlForDisplay('ftp://files.example/private')).toBe('[redacted URL]')
  })
})
