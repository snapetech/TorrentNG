import { describe, expect, it } from 'vitest'
import { maskAnnounceUrl } from './maskUrl'

describe('maskAnnounceUrl', () => {
  it('masks credential query parameters while preserving ordinary values', () => {
    const masked = maskAnnounceUrl(
      'https://tracker.example/announce?passkey=abc123&source=client',
    )

    expect(masked).toContain('passkey=••••••••')
    expect(masked).toContain('source=client')
    expect(masked).not.toContain('abc123')
  })

  it('masks opaque path credentials without hiding the host', () => {
    const masked = maskAnnounceUrl(
      'https://tracker.example/ABC1234567890def/announce',
    )

    expect(masked).toBe('https://tracker.example/••••••••/announce')
  })
})
