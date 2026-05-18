import AxeBuilder from '@axe-core/playwright'
import { expect, test, type Page } from '@playwright/test'

const torrents = Array.from({ length: 40 }, (_, i) => {
  const n = i + 1
  return {
    hash: `a11y${n.toString(16).padStart(36, '0')}`,
    name: `TorrentNG a11y fixture ${n.toString().padStart(3, '0')}`,
    size_bytes: 1024 * 1024 * (700 + n),
    bytes_done: 1024 * 1024 * (700 + n),
    down_rate: 0,
    up_rate: 4096 * n,
    up_total: 1024 * 1024 * n,
    down_total: 1024 * 512 * n,
    ratio: 2500,
    is_active: n % 3 === 0,
    is_open: true,
    complete: true,
    state: 1,
    priority: 0,
    category: n % 2 === 0 ? 'Movies' : 'Linux',
    base_path: `/data/a11y/fixture-${n}`,
    directory: '/data/a11y',
    creation_date: 1_700_000_000 + n,
    timestamp_finished: 1_700_010_000 + n,
    tracker_focus: 0,
    peers_connected: n % 7,
    peers_complete: 100 + n,
    message: '',
    tracker_url: 'https://tracker.example/announce',
    tags: n % 2 === 0 ? 'hd,archive' : 'linux',
    updated_at: 1_700_020_000 + n,
  }
})

async function installA11yApiMock(page: Page) {
  await page.addInitScript(() => {
    try {
      localStorage.clear()
      sessionStorage.clear()
    } catch {
      // Storage can be blocked by browser policy in hardened contexts.
    }
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

  await page.route('**/*', async route => {
    const url = new URL(route.request().url())
    const path = url.pathname
    const json = (body: unknown) => route.fulfill({
      status: 200,
      contentType: 'application/json',
      body: JSON.stringify(body),
    })

    if (path === '/health') return json({ status: 'ok', rtorrent: 'connected', cached_torrents: torrents.length })
    if (path === '/api/qb/v2/transfer/info') return json({ dl_info_speed: 0, up_info_speed: 1048576, dl_info_data: 0, up_info_data: 987654321 })
    if (path === '/api/qb/v2/auth/login' || path === '/api/qb/v2/auth/logout') {
      return route.fulfill({ status: 200, contentType: 'text/plain', body: 'Ok.' })
    }
    if (path === '/api/v1/torrents') {
      const offset = Number(url.searchParams.get('offset') ?? 0)
      const limit = Number(url.searchParams.get('limit') ?? 200)
      return json({ total: torrents.length, torrents: torrents.slice(offset, offset + limit) })
    }
    if (path === '/api/v1/categories') return json([{ name: 'Linux', save_path: '/data/linux', torrent_count: 20 }, { name: 'Movies', save_path: '/data/movies', torrent_count: 20 }])
    if (path === '/api/v1/tags') return json(['archive', 'hd', 'linux'])
    if (path === '/api/v1/storage') {
      return json({ roots: [{ path: '/data/a11y', total_bytes: 10_000_000_000_000, available_bytes: 4_000_000_000_000, used_bytes: 6_000_000_000_000, used_percent: 60, readonly: false, ok: true, error: null }] })
    }
    if (path === '/api/v1/jobs') return json({ jobs: [] })
    if (path === '/api/v1/tracker-health') {
      return json({ trackers: [{ tracker: 'https://tracker.example/announce', torrent_count: torrents.length, active_count: torrents.length, error_count: 0, seed_count: 1024, peer_count: 256, last_updated: 1_700_000_000 }] })
    }
    if (path === '/api/v1/sidebar-facets') {
      return json({ status: { all: torrents.length, seeding: torrents.length, downloading: 0, stopped: 0, checking: 0, error: 0 }, media_type: { video: 20, archive: 20, other: 0 } })
    }
    if (path === '/api/v1/saved-views') return json([])
    if (path === '/api/v1/engine') {
      return json({ mode: 'native', native_engine: true, torrent_count: torrents.length, storage: {}, runtime: {}, resources: { classes: [] }, diagnostics: [] })
    }
    if (path === '/api/v1/engine/commands') return json({ commands: [] })
    if (path === '/api/v1/settings/user-agent') return json({ user_agent: 'TorrentNG/e2e-a11y' })
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
  await installA11yApiMock(page)
  await page.goto('/')
  await expect(page.getByText('TorrentNG').first()).toBeVisible()
})

test('torrent workspace has no serious automated accessibility violations', async ({ page }) => {
  await expect(page.getByText('TorrentNG a11y fixture 001')).toBeVisible()

  const results = await new AxeBuilder({ page })
    .include('body')
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze()
  const serious = results.violations.filter(violation => violation.impact === 'serious' || violation.impact === 'critical')

  expect(serious.map(violation => ({
    id: violation.id,
    impact: violation.impact,
    targets: violation.nodes.flatMap(node => node.target),
  }))).toEqual([])
})

test('settings library panel has no serious automated accessibility violations', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop settings panel exposes the full tabbed layout')

  await page.getByRole('button', { name: 'Settings' }).click()
  await expect(page.getByText('Storage Plan')).toBeVisible()

  const results = await new AxeBuilder({ page })
    .include('body')
    .withTags(['wcag2a', 'wcag2aa', 'wcag21a', 'wcag21aa'])
    .analyze()
  const serious = results.violations.filter(violation => violation.impact === 'serious' || violation.impact === 'critical')

  expect(serious.map(violation => ({
    id: violation.id,
    impact: violation.impact,
    targets: violation.nodes.flatMap(node => node.target),
  }))).toEqual([])
})
