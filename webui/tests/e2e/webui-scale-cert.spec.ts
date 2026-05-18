import { expect, test, type Page } from '@playwright/test'

const SCALE_TOTAL = 15_000
const DEFAULT_LIMIT = 200
const FIRST_VISIBLE_THRESHOLD_MS = Number(process.env.TNG_WEBUI_FIRST_VISIBLE_MS ?? 8000)

function makeTorrent(index: number) {
  const n = index + 1
  const complete = n % 7 !== 0
  return {
    hash: `scale${n.toString(16).padStart(35, '0')}`,
    name: `TorrentNG scale fixture ${n.toString().padStart(5, '0')}`,
    size_bytes: 1024 * 1024 * (500 + (n % 4_000)),
    bytes_done: complete ? 1024 * 1024 * (500 + (n % 4_000)) : 1024 * 1024 * Math.floor((500 + (n % 4_000)) * 0.37),
    down_rate: complete ? 0 : 16_384 + (n % 512) * 1024,
    up_rate: complete ? 32_768 + (n % 2048) * 512 : 2048,
    up_total: 1024 * 1024 * (n % 10_000),
    down_total: 1024 * 512 * (n % 10_000),
    ratio: complete ? 1500 + (n % 900) : 180,
    is_active: n % 5 === 0,
    is_open: n % 13 !== 0,
    complete,
    state: complete ? 1 : 0,
    priority: 0,
    category: n % 3 === 0 ? 'Movies' : n % 3 === 1 ? 'Linux' : 'Archive',
    base_path: `/data/scale/torrent-${n}`,
    directory: '/data/scale',
    creation_date: 1_700_000_000 + n,
    timestamp_finished: complete ? 1_700_100_000 + n : 0,
    tracker_focus: 0,
    peers_connected: n % 23,
    peers_complete: 50 + (n % 300),
    message: '',
    tracker_url: n % 2 === 0 ? 'https://tracker.example/announce' : 'udp://tracker.example:6969/announce',
    tags: n % 2 === 0 ? 'scale,archive' : 'scale,linux',
    updated_at: 1_700_200_000 + n,
  }
}

async function installScaleApiMock(page: Page) {
  await page.addInitScript(() => {
    try {
      localStorage.clear()
      sessionStorage.clear()
    } catch {
      // Browser policy can disable storage. The mocked API still covers auth.
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

    if (path === '/health') {
      return json({ status: 'ok', rtorrent: 'connected', cached_torrents: SCALE_TOTAL })
    }
    if (path === '/api/qb/v2/transfer/info') {
      return json({ dl_info_speed: 524288, up_info_speed: 1048576, dl_info_data: 123456789, up_info_data: 987654321 })
    }
    if (path === '/api/qb/v2/auth/login' || path === '/api/qb/v2/auth/logout') {
      return route.fulfill({ status: 200, contentType: 'text/plain', body: 'Ok.' })
    }
    if (path === '/api/v1/torrents') {
      const offset = Number(url.searchParams.get('offset') ?? 0)
      const limit = Number(url.searchParams.get('limit') ?? DEFAULT_LIMIT)
      const count = Math.max(0, Math.min(limit, SCALE_TOTAL - offset))
      return json({ total: SCALE_TOTAL, torrents: Array.from({ length: count }, (_, i) => makeTorrent(offset + i)) })
    }
    if (path === '/api/v1/categories') {
      return json([
        { name: 'Linux', save_path: '/data/linux', torrent_count: 5000 },
        { name: 'Movies', save_path: '/data/movies', torrent_count: 5000 },
        { name: 'Archive', save_path: '/data/archive', torrent_count: 5000 },
      ])
    }
    if (path === '/api/v1/tags') {
      return json(['archive', 'linux', 'scale'])
    }
    if (path === '/api/v1/storage') {
      return json({
        roots: [
          { path: '/data/scale', total_bytes: 250_000_000_000_000, available_bytes: 75_000_000_000_000, used_bytes: 175_000_000_000_000, used_percent: 70, readonly: false, ok: true, error: null },
        ],
      })
    }
    if (path === '/api/v1/jobs') {
      return json({ jobs: [] })
    }
    if (path === '/api/v1/tracker-health') {
      return json({
        trackers: [{
          tracker: 'https://tracker.example/announce',
          torrent_count: SCALE_TOTAL,
          active_count: 3000,
          error_count: 0,
          seed_count: 300_000,
          peer_count: 24_000,
          last_updated: 1_700_000_000,
        }],
      })
    }
    if (path === '/api/v1/sidebar-facets') {
      return json({
        status: { all: SCALE_TOTAL, seeding: 12_857, downloading: 2143, stopped: 0, checking: 0, error: 0 },
        media_type: { video: 5000, archive: 5000, other: 5000 },
      })
    }
    if (path === '/api/v1/saved-views') {
      return json([])
    }
    if (path === '/api/v1/engine') {
      return json({
        mode: 'native',
        native_engine: true,
        torrent_count: SCALE_TOTAL,
        storage: {},
        runtime: {},
        resources: { classes: [] },
        diagnostics: [],
      })
    }
    if (path === '/api/v1/engine/commands') {
      return json({ commands: [] })
    }
    if (path === '/api/v1/settings/user-agent') {
      return json({ user_agent: 'TorrentNG/e2e-scale' })
    }
    if (path === '/api/v1/ratio-groups' || path === '/api/v1/workflows' || path === '/api/v1/workflow-runs' || path === '/api/v1/rss-rules') {
      return json([])
    }
    if (path === '/api/v1/logs') {
      return json({ logs: [] })
    }
    if (path.startsWith('/api/v1/torrents/') && path.endsWith('/trackers')) {
      return json({ trackers: [{ url: 'https://tracker.example/announce', is_enabled: true, success_counter: 12, failed_counter: 0, scrape_complete: 100, scrape_incomplete: 2, message: 'ok' }] })
    }
    if (path.startsWith('/api/v1/torrents/') && path.endsWith('/files')) {
      return json({ files: [{ index: 0, path: 'fixture.bin', size_bytes: 1024 * 1024, completed_chunks: 64, size_chunks: 64, priority: 1, is_created: true }] })
    }
    if (path.startsWith('/api/v1/') || path.startsWith('/api/qb/v2/')) {
      return json({})
    }
    return route.continue()
  })
}

test.beforeEach(async ({ page }) => {
  const errors: string[] = []
  page.on('pageerror', err => errors.push(err.message))
  page.on('console', msg => {
    if (msg.type() === 'error') errors.push(msg.text())
  })
  await installScaleApiMock(page)
  await page.goto('/')
  await expect(page.getByText('TorrentNG').first()).toBeVisible()
  await expect.poll(() => errors, { message: 'no browser console/page errors' }).toEqual([])
})

test('desktop handles 15k torrents without rendering every row', async ({ page, isMobile }) => {
  test.skip(isMobile, 'desktop-only scale assertion')

  const startedAt = Date.now()
  await expect(page.getByText('TorrentNG scale fixture 00001')).toBeVisible()
  expect(Date.now() - startedAt).toBeLessThan(FIRST_VISIBLE_THRESHOLD_MS)
  await expect(page.getByText('15,000 torrents')).toBeVisible()

  const renderedRows = await page.locator('.torrent-row').count()
  expect(renderedRows).toBeGreaterThan(0)
  expect(renderedRows).toBeLessThan(120)

  await expect(page.getByRole('button', { name: /200 \/ 15,000/ })).toBeVisible()
  await page.getByRole('button', { name: /200 \/ 15,000/ }).click()
  await expect(page.getByRole('button', { name: /400 \/ 15,000/ })).toBeVisible()
})

test('core workspace controls expose accessible names', async ({ page }) => {
  await expect(page.getByRole('navigation', { name: 'Primary' })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Settings' })).toBeVisible()
  await expect(page.getByLabel('Select all visible torrents')).toBeVisible()
  await expect(page.getByRole('button', { name: /Sort by Name/i })).toBeVisible()
  await expect(page.getByRole('button', { name: 'Choose visible table columns' })).toBeVisible()

  const unnamedControls = await page.locator('main button, main input, main select, main textarea, main [role="button"]').evaluateAll(elements => elements
    .filter(element => {
      const rect = element.getBoundingClientRect()
      if (rect.width === 0 || rect.height === 0) return false
      const style = window.getComputedStyle(element)
      if (style.visibility === 'hidden' || style.display === 'none') return false
      const labelledBy = element.getAttribute('aria-labelledby')
      const labelText = labelledBy
        ?.split(/\s+/)
        .map(id => document.getElementById(id)?.textContent?.trim() ?? '')
        .join(' ')
        .trim()
      const labels = 'labels' in element
        ? Array.from((element as HTMLInputElement).labels ?? []).map(label => label.textContent?.trim() ?? '').join(' ').trim()
        : ''
      const name = [
        element.getAttribute('aria-label'),
        element.getAttribute('title'),
        labelText,
        labels,
        element.getAttribute('placeholder'),
        element.textContent,
      ].find(value => value && value.trim().length > 0)
      return !name
    })
    .map(element => `${element.tagName.toLowerCase()}${element.id ? `#${element.id}` : ''}${element.className ? `.${String(element.className).replace(/\s+/g, '.')}` : ''}`))

  expect(unnamedControls).toEqual([])

  await page.keyboard.press('Tab')
  await expect.poll(() => page.evaluate(() => document.activeElement?.tagName ?? '')).not.toBe('BODY')
})
