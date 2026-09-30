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

  it('normalizes malformed compatibility numbers before they reach the UI', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      total: 1,
      torrents: [{
        info_hash: 'a'.repeat(40),
        name: 'broken numeric payload',
        state: 'downloading',
        total_length: 'not-a-number',
        downloaded: 'Infinity',
        uploaded: -1,
        ratio: 'NaN',
        save_path: '/downloads',
        category: null,
        tags: [],
        added_at: 'NaN',
        completed_at: null,
        num_peers: 'Infinity',
        num_seeds: -4,
      }],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })))

    const result = await api.torrents.list()
    const [torrent] = result.torrents

    expect(torrent.size_bytes).toBe(0)
    expect(torrent.bytes_done).toBe(0)
    expect(torrent.up_total).toBe(0)
    expect(torrent.ratio).toBe(0)
    expect(torrent.creation_date).toBe(0)
    expect(torrent.peers_connected).toBe(0)
    expect(torrent.peers_complete).toBe(0)
    expect(Object.values(torrent).every(value => typeof value !== 'number' || Number.isFinite(value))).toBe(true)
  })

  it('saturates a finite compatibility ratio that would overflow when scaled', async () => {
    vi.stubGlobal('fetch', vi.fn().mockResolvedValue(new Response(JSON.stringify({
      torrents: [{
        info_hash: 'b'.repeat(40),
        name: 'large ratio',
        state: 'seeding',
        total_length: 1,
        downloaded: 1,
        uploaded: 1,
        ratio: '1e308',
        save_path: '/downloads',
        tags: [],
      }],
    }), { status: 200, headers: { 'Content-Type': 'application/json' } })))

    const result = await api.torrents.list()
    const [torrent] = result.torrents

    expect(torrent.ratio).toBe(Number.MAX_SAFE_INTEGER)
    expect(Number.isFinite(torrent.ratio)).toBe(true)
  })
})
