import { afterEach, describe, expect, it, vi } from 'vitest'

describe('YunoHost URL prefix support', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    delete window.__TNG_BASE_PATH__
    vi.resetModules()
  })

  it('prefixes API requests with the configured mount path', async () => {
    window.__TNG_BASE_PATH__ = '/torrentng/'
    vi.resetModules()
    const fetchMock = vi.fn().mockResolvedValue(new Response('Ok.'))
    vi.stubGlobal('fetch', fetchMock)
    const { api, appBasePath } = await import('./client')

    expect(appBasePath).toBe('/torrentng')
    await api.auth.login('admin', 'a-secret-token')

    expect(fetchMock).toHaveBeenCalledWith(
      '/torrentng/api/v1/auth/login',
      expect.objectContaining({ method: 'POST' }),
    )
  })
})
