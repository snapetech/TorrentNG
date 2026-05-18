import { expect, test, type Page } from '@playwright/test'

const torrents = Array.from({ length: 80 }, (_, i) => {
  const n = i + 1
  const complete = n % 6 !== 0
  return {
    hash: `visual${n.toString(16).padStart(34, '0')}`,
    name: `TorrentNG visual fixture ${n.toString().padStart(3, '0')}`,
    size_bytes: 1024 * 1024 * (900 + n),
    bytes_done: complete ? 1024 * 1024 * (900 + n) : 1024 * 1024 * Math.floor((900 + n) * 0.44),
    down_rate: complete ? 0 : 4096 * n,
    up_rate: complete ? 8192 * n : 512,
    up_total: 1024 * 1024 * n,
    down_total: 1024 * 512 * n,
    ratio: complete ? 2200 : 410,
    is_active: n % 4 === 0,
    is_open: n % 5 !== 0,
    complete,
    state: complete ? 1 : 0,
    priority: 0,
    category: n % 2 === 0 ? 'Movies' : 'Linux',
    base_path: `/data/visual/fixture-${n}`,
    directory: '/data/visual',
    creation_date: 1_700_000_000 + n,
    timestamp_finished: complete ? 1_700_010_000 + n : 0,
    tracker_focus: 0,
    peers_connected: n % 13,
    peers_complete: 100 + n,
    message: n === 7 ? 'Tracker warning fixture' : '',
    tracker_url: n % 2 === 0 ? 'https://tracker.example/announce' : 'udp://tracker.example:6969/announce',
    tags: n % 2 === 0 ? 'hd,archive' : 'linux',
    updated_at: 1_700_020_000 + n,
  }
})

async function installVisualApiMock(page: Page) {
  await page.addInitScript(() => {
    try {
      localStorage.clear()
      sessionStorage.clear()
      localStorage.setItem('tng:theme:id', 'torrentng')
      localStorage.setItem('tng:theme:mode', 'dark')
    } catch {
      // Browser storage can be blocked by policy; the default theme is stable.
    }
    Date.now = () => 1_700_030_000_000
    class NoopWebSocket extends EventTarget {
      static readonly CONNECTING = 0
      static readonly OPEN = 1
      static readonly CLOSING = 2
      static readonly CLOSED = 3
      readonly CONNECTING = 0
      readonly OPEN = 1
      readonly CLOSING = 2
      readonly CLOSED = 3
      readyState = 1
      binaryType: BinaryType = 'blob'
      bufferedAmount = 0
      extensions = ''
      protocol = ''
      url = ''
      onopen: ((this: WebSocket, ev: Event) => unknown) | null = null
      onmessage: ((this: WebSocket, ev: MessageEvent) => unknown) | null = null
      onerror: ((this: WebSocket, ev: Event) => unknown) | null = null
      onclose: ((this: WebSocket, ev: CloseEvent) => unknown) | null = null
      constructor(url: string | URL) {
        super()
        this.url = String(url)
        setTimeout(() => this.onopen?.call(this as unknown as WebSocket, new Event('open')), 0)
      }
      send() {}
      close() {
        this.readyState = 3
        this.onclose?.call(this as unknown as WebSocket, new CloseEvent('close'))
      }
    }
    window.WebSocket = NoopWebSocket as unknown as typeof WebSocket
  })

  await page.addStyleTag({
    content: `
      *, *::before, *::after {
        animation-duration: 0s !important;
        animation-delay: 0s !important;
        transition-duration: 0s !important;
        caret-color: transparent !important;
      }
    `,
  })

  await page.route('**/*', async route => {
    const url = new URL(route.request().url())
    const path = url.pathname
    const json = (body: unknown) => route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(body),
    })

    if (path === '/health') return json({ status: 'ok', rtorrent: 'connected', cached_torrents: torrents.length })
    if (path === '/api/qb/v2/transfer/info') return json({ dl_info_speed: 262144, up_info_speed: 1048576, dl_info_data: 123456789, up_info_data: 987654321 })
    if (path === '/api/qb/v2/auth/login' || path === '/api/qb/v2/auth/logout') {
      return route.fulfill({ status: 200, contentType: 'text/plain', body: 'Ok.' })
    }
    if (path === '/api/v1/torrents') {
      const offset = Number(url.searchParams.get('offset') ?? 0)
      const limit = Number(url.searchParams.get('limit') ?? 200)
      return json({ total: torrents.length, torrents: torrents.slice(offset, offset + limit) })
    }
    if (path === '/api/v1/categories') return json([{ name: 'Linux', save_path: '/data/linux', torrent_count: 40 }, { name: 'Movies', save_path: '/data/movies', torrent_count: 40 }])
    if (path === '/api/v1/tags') return json(['archive', 'hd', 'linux'])
    if (path === '/api/v1/storage') {
      return json({ roots: [{ path: '/data/visual', total_bytes: 10_000_000_000_000, available_bytes: 4_000_000_000_000, used_bytes: 6_000_000_000_000, used_percent: 60, readonly: false, ok: true, error: null }] })
    }
    if (path === '/api/v1/jobs') return json({ jobs: [] })
    if (path === '/api/v1/tracker-health') {
      return json({ trackers: [{ tracker: 'https://tracker.example/announce', torrent_count: 80, active_count: 64, error_count: 2, seed_count: 1024, peer_count: 256, last_updated: 1_700_000_000 }] })
    }
    if (path === '/api/v1/sidebar-facets') {
      return json({ status: { all: 80, seeding: 67, downloading: 13, stopped: 0, checking: 0, error: 0 }, media_type: { video: 40, archive: 20, other: 20 } })
    }
    if (path === '/api/v1/saved-views') return json([{ id: 'visual-linux', name: 'Linux seeders', params: { category: 'Linux', status: 'seeding' } }])
    if (path === '/api/v1/engine') {
      return json({ mode: 'native', native_engine: true, torrent_count: torrents.length, storage: {}, runtime: {}, resources: { classes: [] }, diagnostics: [] })
    }
    if (path === '/api/v1/engine/commands') return json({ commands: [] })
    if (path === '/api/v1/settings/user-agent') return json({ user_agent: 'TorrentNG/e2e-visual' })
    if (path === '/api/v1/ratio-groups' || path === '/api/v1/workflows' || path === '/api/v1/workflow-runs' || path === '/api/v1/rss-rules') return json([])
    if (path === '/api/v1/logs') return json({ logs: [] })
    if (path.startsWith('/api/v1/torrents/') && path.endsWith('/trackers')) {
      return json({ trackers: [{ url: 'https://tracker.example/announce', is_enabled: true, success_counter: 12, failed_counter: 0, scrape_complete: 100, scrape_incomplete: 2, message: 'ok' }] })
    }
    if (path.startsWith('/api/v1/torrents/') && path.endsWith('/files')) {
      return json({ files: [{ index: 0, path: 'fixture.bin', size_bytes: 1024 * 1024, completed_chunks: 64, size_chunks: 64, priority: 1, is_created: true }] })
    }
    if (path.startsWith('/api/v1/') || path.startsWith('/api/qb/v2/')) return json({})
    return route.continue()
  })
}

test.beforeEach(async ({ page }) => {
  await installVisualApiMock(page)
  await page.goto('/')
  await expect(page.getByText('TorrentNG visual fixture 001')).toBeVisible()
})

test('torrent workspace visual baseline', async ({ page }) => {
  await expect(page).toHaveScreenshot('torrent-workspace.png', {
    fullPage: true,
    maxDiffPixelRatio: 0.01,
  })
})

test('settings storage visual baseline', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop settings baseline captures the full storage planner layout')

  await page.getByRole('button', { name: 'Settings' }).click()
  await expect(page.getByText('Storage Plan')).toBeVisible()
  await expect(page).toHaveScreenshot('settings-storage.png', {
    fullPage: true,
    maxDiffPixelRatio: 0.01,
  })
})
