const BASE = '/api/v1'

export interface TorrentSummary {
  hash: string
  name: string
  size_bytes: number
  bytes_done: number
  down_rate: number
  up_rate: number
  up_total: number
  down_total: number
  ratio: number
  is_active: boolean
  is_open: boolean
  complete: boolean
  state: number
  priority: number
  category: string
  base_path: string
  directory: string
  creation_date: number
  timestamp_finished: number
  tracker_focus: number
  peers_connected: number
  peers_complete: number
  message: string
  tracker_url: string
  tags: string
  updated_at: number
}

export interface TorrentListResponse {
  total: number
  torrents: TorrentSummary[]
}

export interface ListParams {
  filter?: string
  status?: string
  category?: string
  tag?: string
  sort?: string
  dir?: 'asc' | 'desc'
  limit?: number
  offset?: number
}

async function get<T>(path: string, params?: Record<string, string | number>): Promise<T> {
  const url = new URL(BASE + path, window.location.origin)
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined && v !== '') url.searchParams.set(k, String(v))
    }
  }
  const res = await fetch(url)
  if (!res.ok) throw new Error(`API ${res.status}: ${url.pathname}`)
  return res.json()
}

async function post(path: string, body?: FormData | Record<string, string>): Promise<void> {
  const res = await fetch(BASE + path, {
    method: 'POST',
    headers: body instanceof FormData ? undefined : { 'Content-Type': 'application/json', 'X-RTNG-CSRF': '1' },
    body: body instanceof FormData ? body : JSON.stringify(body),
  })
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
}

async function del(path: string): Promise<void> {
  const res = await fetch(BASE + path, {
    method: 'DELETE',
    headers: { 'X-RTNG-CSRF': '1' },
  })
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
}

export const api = {
  torrents: {
    list: (p: ListParams = {}): Promise<TorrentListResponse> =>
      get('/torrents', p as Record<string, string | number>),

    start: (hash: string) => post(`/torrents/${hash}/start`),
    stop:  (hash: string) => post(`/torrents/${hash}/stop`),
    recheck: (hash: string) => post(`/torrents/${hash}/recheck`),
    reannounce: (hash: string) => post(`/torrents/${hash}/reannounce`),
    remove: (hash: string, deleteFiles = false) =>
      del(`/torrents/${hash}?delete_files=${deleteFiles}`),

    addMagnet: (magnet: string, savePath = '') => {
      const fd = new FormData()
      fd.append('magnet', magnet)
      fd.append('save_path', savePath)
      return post('/torrents', fd)
    },
  },

  health: (): Promise<{ status: string; rtorrent: string; cached_torrents: number }> =>
    get('/health' as any),
}
