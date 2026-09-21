import { afterEach, describe, expect, it, vi } from 'vitest'
import { api } from './client'

describe('authentication API', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
  })

  it('shows the login retry delay when the server throttles attempts', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('Fails.', {
      status: 429,
      headers: { 'Retry-After': '60' },
    })))

    await expect(api.auth.login('admin', 'wrong')).rejects.toMatchObject({
      name: 'AuthError',
      message: 'Too many login attempts. Try again in 60 seconds.',
    })
  })

  it('uses a generic retry message when Retry-After is missing or invalid', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response('Fails.', {
      status: 429,
      headers: { 'Retry-After': 'later' },
    })))

    await expect(api.auth.login('admin', 'wrong')).rejects.toMatchObject({
      name: 'AuthError',
      message: 'Too many login attempts. Please try again later.',
    })
  })
})
