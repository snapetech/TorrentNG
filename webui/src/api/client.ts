const BASE = '/api/v1'

export class AuthError extends Error {
  constructor(message = 'Unauthorized') {
    super(message)
    this.name = 'AuthError'
  }
}

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
  tracker?: string
  media_type?: string
  sort?: string
  dir?: 'asc' | 'desc'
  limit?: number
  offset?: number
}

async function get<T>(path: string, params?: Record<string, string | number | undefined>): Promise<T> {
  const url = new URL(BASE + path, window.location.origin)
  if (params) {
    for (const [k, v] of Object.entries(params)) {
      if (v !== undefined && v !== '') url.searchParams.set(k, String(v))
    }
  }
  const res = await fetch(url, { credentials: 'same-origin' })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${url.pathname}`)
  return res.json()
}

async function getRoot<T>(path: string): Promise<T> {
  const res = await fetch(path, { credentials: 'same-origin' })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
  return res.json()
}

async function post<T = void>(path: string, body?: FormData | object): Promise<T> {
  const isForm = body instanceof FormData
  const res = await fetch(BASE + path, {
    method: 'POST',
    headers: isForm ? undefined : { 'Content-Type': 'application/json', 'X-TNG-CSRF': '1' },
    body: isForm ? body : body !== undefined ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
  const ct = res.headers.get('content-type') ?? ''
  if (ct.includes('application/json')) return res.json() as Promise<T>
  return undefined as T
}

async function put<T = void>(path: string, body: object): Promise<T> {
  const res = await fetch(BASE + path, {
    method: 'PUT',
    headers: { 'Content-Type': 'application/json', 'X-TNG-CSRF': '1' },
    body: JSON.stringify(body),
    credentials: 'same-origin',
  })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
  const ct = res.headers.get('content-type') ?? ''
  if (ct.includes('application/json')) return res.json() as Promise<T>
  return undefined as T
}

async function patch<T = void>(path: string, body: object): Promise<T> {
  const res = await fetch(BASE + path, {
    method: 'PATCH',
    headers: { 'Content-Type': 'application/json', 'X-TNG-CSRF': '1' },
    body: JSON.stringify(body),
    credentials: 'same-origin',
  })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
  const ct = res.headers.get('content-type') ?? ''
  if (ct.includes('application/json')) return res.json() as Promise<T>
  return undefined as T
}

async function del(path: string, body?: object): Promise<void> {
  const res = await fetch(BASE + path, {
    method: 'DELETE',
    headers: body ? { 'Content-Type': 'application/json', 'X-TNG-CSRF': '1' } : { 'X-TNG-CSRF': '1' },
    body: body ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
}

async function delJson<T>(path: string, body?: object): Promise<T> {
  const res = await fetch(BASE + path, {
    method: 'DELETE',
    headers: body ? { 'Content-Type': 'application/json', 'X-TNG-CSRF': '1' } : { 'X-TNG-CSRF': '1' },
    body: body ? JSON.stringify(body) : undefined,
    credentials: 'same-origin',
  })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
  return res.json() as Promise<T>
}

async function login(username: string, password: string): Promise<void> {
  const form = new URLSearchParams()
  form.set('username', username)
  form.set('password', password)
  const res = await fetch('/api/qb/v2/auth/login', {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded' },
    body: form,
    credentials: 'same-origin',
  })
  const text = await res.text()
  if (!res.ok || text.trim() !== 'Ok.') {
    throw new AuthError('Invalid username or password')
  }
}

async function logout(): Promise<void> {
  await fetch('/api/qb/v2/auth/logout', {
    method: 'POST',
    credentials: 'same-origin',
  })
}

async function qbPost(path: string, fields: Record<string, string | number | boolean | undefined | null>): Promise<void> {
  const form = new URLSearchParams()
  for (const [key, value] of Object.entries(fields)) {
    if (value !== undefined && value !== null) form.set(key, String(value))
  }
  const res = await fetch('/api/qb/v2' + path, {
    method: 'POST',
    headers: { 'Content-Type': 'application/x-www-form-urlencoded', 'X-TNG-CSRF': '1' },
    body: form,
    credentials: 'same-origin',
  })
  if (res.status === 401) throw new AuthError()
  if (!res.ok) throw new Error(`API ${res.status}: ${path}`)
}

export interface Category {
  name: string
  save_path: string
  torrent_count?: number
}

export interface BulkResult {
  applied: string[]
  errors: string[]
  dry_run: boolean
}

export interface BulkOptions {
  category?: string
  save_path?: string
}

export interface Tracker {
  url: string
  is_enabled: boolean
  success_counter: number
  failed_counter: number
  scrape_complete: number
  scrape_incomplete: number
  message: string
}

export interface StorageRoot {
  path: string
  total_bytes: number
  available_bytes: number
  used_bytes: number
  used_percent: number
  readonly: boolean
  ok: boolean
  error: string | null
}

export interface StorageResponse {
  roots: StorageRoot[]
}

export interface StoragePlanRequest {
  operation: 'move' | 'import' | 'delete'
  source?: string | null
  destination?: string | null
  target?: string | null
  bytes?: number | null
  available_bytes?: number | null
  hardlink_or_copy?: boolean | null
  dry_run?: boolean | null
  dry_run_approved?: boolean | null
  roots?: string[] | null
  affected_torrents?: string[] | null
  completed_steps?: number[] | null
}

export interface StoragePlanStep {
  action: string
  source: string | null
  destination: string | null
  bytes: number
}

export interface StoragePlanView {
  dry_run: boolean
  can_apply: boolean
  issues: string[]
  steps: StoragePlanStep[]
  rollback_steps: StoragePlanStep[]
}

export interface StoragePlanResponse {
  operation: string
  job_id: string | null
  plan: StoragePlanView
}

export interface Job {
  job_id: string
  kind: string
  state: string
  dry_run: boolean
  affected_torrents: string[]
  total: number
  done: number
  checkpoint: number
  byte_offset: number | null
  verified_bytes: number
  error: string | null
  created_at: number
  started_at: number | null
  updated_at: number
  finished_at: number | null
  progress: number
}

export interface JobsResponse {
  jobs: Job[]
}

export interface LiveStats {
  upload_speed: number
  download_speed: number
  upload_total?: number
  download_total?: number
  connections?: number
  pending_connections?: number
  listen_port?: number
  firewall?: 'open' | 'listening' | 'closed' | 'unknown' | string
  dht?: 'on' | 'off' | 'unknown' | string
  pex?: 'on' | 'off' | 'unknown' | string
}

export interface SessionFeatureResponse {
  dht?: boolean
  pex?: boolean
}

export interface TransferInfo {
  dl_info_speed: number
  dl_info_data?: number
  up_info_speed: number
  up_info_data?: number
}

export interface TrackerHealth {
  tracker: string
  torrent_count: number
  active_count: number
  complete_count: number
  error_count: number
  seed_count: number
  peer_count: number
  last_updated: number
}

export interface TrackerHealthResponse {
  trackers: TrackerHealth[]
}

export interface AppLogEvent {
  event_id: number | null
  occurred_at: number
  level: 'info' | 'warn' | 'warning' | 'error' | string
  kind: string
  message: string
  payload: string
}

export interface LogsResponse {
  logs: AppLogEvent[]
}

export interface SidebarFacets {
  status: Record<string, number>
  media_type: Record<string, number>
}

export interface ProbeValue<T> {
  ok: boolean
  value: T | null
  error: string | null
}

export interface EngineCapability {
  key: string
  label: string
  command: string
  available: boolean
  detail: string | null
}

export interface BackendCapabilities {
  supports_tags: boolean
  supports_categories: boolean
  supports_file_priority: boolean
  supports_tracker_edit: boolean
  supports_recheck: boolean
  supports_runtime_user_agent: boolean
  supports_config_overlay: boolean
  supports_restart: boolean
}

export interface BackendInfo {
  type: string
  status?: string
  capabilities: BackendCapabilities
}

export interface EngineDiagnostics {
  backend: BackendInfo
  provenance: {
    sidecar_version: string
    rtorrent_version: string | null
    libtorrent_version: string | null
    xmlrpc_backend: string
    packaged_rtorrent_version: string | null
    packaged_libtorrent_version: string | null
    patch_set: string[]
  }
  capabilities: EngineCapability[]
  http: {
    user_agent: ProbeValue<string>
    current_open: ProbeValue<number>
    max_total_connections: ProbeValue<number>
    max_host_connections: ProbeValue<number>
    max_cache_connections: ProbeValue<number>
    dns_cache_timeout: ProbeValue<number>
    proxy_address: ProbeValue<string>
    ca_path: ProbeValue<string>
    ca_cert: ProbeValue<string>
    ssl_verify_peer: ProbeValue<boolean>
    ssl_verify_host: ProbeValue<boolean>
  }
  dht: {
    enabled: ProbeValue<string>
    port: ProbeValue<number>
    override_port: ProbeValue<number>
    listen_port: ProbeValue<number>
    listen_range: ProbeValue<string>
    pex: ProbeValue<boolean>
    udp_trackers: ProbeValue<boolean>
    statistics: ProbeValue<string>
  }
  drift: EngineDrift[]
}

export interface HealthResponse {
  status: string
  backend?: {
    type: string
    status: string
  }
  rtorrent: string
  cached_torrents: number
}

export interface EngineDrift {
  key: string
  label: string
  command: string
  expected: string
  actual: string | null
  status: 'match' | 'mismatch' | 'unavailable'
  detail: string | null
}

export interface EngineCommandIndex {
  ok: boolean
  count: number
  commands: string[]
  error: string | null
}

export interface RtorrentSettingDescriptor {
  key: string
  label: string
  command: string
  setter: string
  value_type: 'int' | 'bool' | 'enum' | string
  unit: string | null
  restart_required: boolean
  minimum: number | null
  maximum: number | null
  default_value: string | number | boolean
}

export interface RtorrentSettingState {
  key: string
  live: ProbeValue<string>
  saved: string | null
}

export interface RtorrentSettingsResponse {
  settings: RtorrentSettingDescriptor[]
  values: RtorrentSettingState[]
  overlay_path: string
  overlay_writable: boolean
  custom_rc: string
  restart_supported: boolean
}

export interface RtorrentSettingsApplyResponse {
  saved: boolean
  restart_required: boolean
  applied: string[]
  errors: string[]
  overlay_path: string
}

export interface RatioGroup {
  name: string
  ratio_limit: number
  seeding_time_limit: number
  category: string | null
  tracker: string | null
  enabled: boolean
}

export interface WorkflowRule {
  id: string
  name: string
  enabled: boolean
  event: 'completed' | 'added' | 'category_changed'
  action: 'webhook' | 'script' | 'set_category' | 'set_location'
  category: string | null
  tracker: string | null
  command: string | null
  url: string | null
  target_path: string | null
}

export interface WorkflowRun {
  id: string
  rule_id: string
  rule_name: string
  action: WorkflowRule['action']
  dry_run: boolean
  matched: string[]
  applied: string[]
  errors: string[]
  started_at: number
}

export interface SavedView {
  id: string
  name: string
  params: Omit<ListParams, 'limit' | 'offset'>
}

export interface RssRule {
  id: string
  name: string
  enabled: boolean
  feed_url: string
  include: string
  exclude: string | null
  category: string | null
  save_path: string | null
  tags: string[]
  start: boolean
}

export interface RssRuleMatch {
  rule_id: string
  rule_name: string
  matched: boolean
  reason: string
  category: string | null
  save_path: string | null
  tags: string[]
  start: boolean
}

export interface TrackerEdit {
  orig_url: string
  new_url: string
}

export interface TrackerPatch {
  add?: string[]
  remove?: string[]
  edit?: TrackerEdit[]
}

export interface TorrentFile {
  index: number
  path: string
  size_bytes: number
  completed_chunks: number
  size_chunks: number
  priority: number
  is_created: boolean
}

interface TrackersResponse {
  trackers: Tracker[]
}

interface FilesResponse {
  files: TorrentFile[]
}

export const api = {
  auth: {
    login,
    logout,
    check: (): Promise<TorrentListResponse> => get('/torrents', { limit: 1 }),
  },

  torrents: {
    list: (p: ListParams = {}): Promise<TorrentListResponse> =>
      get('/torrents', p as Record<string, string | number>),

    get: (hash: string): Promise<TorrentSummary> =>
      get(`/torrents/${hash}`),

    start:      (hash: string) => post(`/torrents/${hash}/start`),
    stop:       (hash: string) => post(`/torrents/${hash}/stop`),
    recheck:    (hash: string) => post(`/torrents/${hash}/recheck`),
    reannounce: (hash: string) => post(`/torrents/${hash}/reannounce`),
    remove: (hash: string, deleteFiles = false) =>
      del(`/torrents/${hash}?delete_files=${deleteFiles}`),

    update: (hash: string, body: { save_path?: string }) =>
      put(`/torrents/${hash}`, body),

    rename: (hash: string, name: string) =>
      qbPost('/torrents/rename', { hash, name }),

    setLocation: (hashes: string[], location: string) =>
      qbPost('/torrents/setLocation', { hashes: hashes.join('|'), location }),

    setShareLimits: (hashes: string[], ratioLimit: number, seedingTimeLimit: number) =>
      qbPost('/torrents/setShareLimits', {
        hashes: hashes.join('|'),
        ratioLimit,
        seedingTimeLimit,
      }),

    toggleSequential: (hashes: string[]) =>
      qbPost('/torrents/toggleSequentialDownload', { hashes: hashes.join('|') }),

    trackers: async (hash: string): Promise<Tracker[]> =>
      (await get<TrackersResponse>(`/torrents/${hash}/trackers`)).trackers,

    patchTrackers: (hash: string, body: TrackerPatch) =>
      patch(`/torrents/${hash}/trackers`, body),

    files: async (hash: string): Promise<TorrentFile[]> =>
      (await get<FilesResponse>(`/torrents/${hash}/files`)).files,

    setCategory: (hash: string, category: string) =>
      put(`/torrents/${hash}/category`, { category }),

    addTags: (hash: string, tags: string[]) =>
      post(`/torrents/${hash}/tags`, { tags }),

    removeTags: (hash: string, tags: string[]) =>
      del(`/torrents/${hash}/tags`, { tags }),

    setTags: (hashes: string[], tags: string[]) =>
      qbPost('/torrents/setTags', { hashes: hashes.join('|'), tags: tags.join(',') }),

    setFilePriority: (hash: string, fileIds: number[], priority: number) =>
      qbPost('/torrents/filePrio', { hash, id: fileIds.join('|'), priority }),

    renameFile: (hash: string, id: number, name: string) =>
      qbPost('/torrents/renameFile', { hash, id, name }),

    addFile: (file: File, savePath = '', category = '', start = true) => {
      const fd = new FormData()
      fd.append('torrent', file)
      if (savePath) fd.append('save_path', savePath)
      if (category) fd.append('category', category)
      fd.append('start', start ? 'true' : 'false')
      return post('/torrents', fd)
    },

    addMagnet: (magnet: string, savePath = '', category = '', start = true) => {
      const fd = new FormData()
      fd.append('magnet', magnet)
      if (savePath) fd.append('save_path', savePath)
      if (category) fd.append('category', category)
      fd.append('start', start ? 'true' : 'false')
      return post('/torrents', fd)
    },
  },

  categories: {
    list: (): Promise<Category[]> => get('/categories'),
    create: (name: string, save_path: string) => post('/categories', { name, save_path }),
    delete: (name: string) => del(`/categories/${encodeURIComponent(name)}`),
  },

  tags: {
    list: (): Promise<string[]> => get('/tags'),
    create: (name: string) => post('/tags', { name }),
    delete: (name: string) => del(`/tags/${encodeURIComponent(name)}`),
  },

  bulk: (
    action: 'start' | 'stop' | 'recheck' | 'reannounce' | 'set-category' | 'set-location',
    hashes: string[],
    dry_run = false,
    options: BulkOptions = {},
  ): Promise<BulkResult> =>
    post(`/bulk/${action}`, { hashes, dry_run, ...options }),

  crossSeed: (
    hashes: string[],
    trackers: string[],
    reannounce = true,
    dry_run = true,
  ): Promise<BulkResult> =>
    post('/cross-seed', { hashes, trackers, reannounce, dry_run }),

  settings: {
    getUserAgent: (): Promise<{ user_agent: string }> => get('/settings/user-agent'),
    setUserAgent: (user_agent: string) => put('/settings/user-agent', { user_agent }),
  },

  storage: (): Promise<StorageResponse> => get('/storage'),
  jobs: (): Promise<JobsResponse> => get('/jobs'),
  storagePlan: {
    preview: (body: StoragePlanRequest): Promise<StoragePlanResponse> =>
      post('/storage/plan', body),
    execute: (body: StoragePlanRequest): Promise<StoragePlanResponse> =>
      post('/storage/execute', body),
  },

  session: {
    setFeatures: (features: { dht?: boolean; pex?: boolean }): Promise<SessionFeatureResponse> =>
      patch('/session/features', features),
  },

  transferInfo: (): Promise<TransferInfo> => getRoot('/api/qb/v2/transfer/info'),

  trackerHealth: (): Promise<TrackerHealthResponse> => get('/tracker-health'),
  sidebarFacets: (): Promise<SidebarFacets> => get('/sidebar-facets'),
  logs: (params: {
    limit?: number
    kind?: string
    level?: string
    last_known_id?: number
  } = {}): Promise<LogsResponse> => get('/logs', params),

  engine: (): Promise<EngineDiagnostics> => get('/engine'),
  engineCommands: (): Promise<EngineCommandIndex> => get('/engine/commands'),
  rtorrentSettings: {
    get: (): Promise<RtorrentSettingsResponse> => get('/engine/rtorrent-settings'),
    save: (
      values: Record<string, string | number | boolean>,
      custom_rc = '',
      apply_live = true,
    ): Promise<RtorrentSettingsApplyResponse> =>
      put('/engine/rtorrent-settings', { values, custom_rc, apply_live }),
    restart: (): Promise<{ restarting: boolean }> => post('/engine/restart'),
  },

  savedViews: {
    list: (): Promise<SavedView[]> => get('/saved-views'),
    save: (view: SavedView): Promise<SavedView[]> => post('/saved-views', view),
    delete: (id: string): Promise<SavedView[]> => delJson(`/saved-views/${encodeURIComponent(id)}`),
  },

  ratioGroups: {
    list: (): Promise<RatioGroup[]> => get('/ratio-groups'),
    save: (group: RatioGroup): Promise<RatioGroup[]> => post('/ratio-groups', group),
    delete: (name: string): Promise<RatioGroup[]> => delJson(`/ratio-groups/${encodeURIComponent(name)}`),
    apply: (name: string, dry_run = false): Promise<BulkResult> =>
      post(`/ratio-groups/${encodeURIComponent(name)}`, { dry_run }),
  },

  workflows: {
    list: (): Promise<WorkflowRule[]> => get('/workflows'),
    runs: (): Promise<WorkflowRun[]> => get('/workflow-runs'),
    save: (rule: WorkflowRule): Promise<WorkflowRule[]> => post('/workflows', rule),
    run: (id: string, dry_run = false): Promise<BulkResult> =>
      post(`/workflows/${encodeURIComponent(id)}`, { dry_run }),
    delete: (id: string): Promise<WorkflowRule[]> => delJson(`/workflows/${encodeURIComponent(id)}`),
  },

  rssRules: {
    list: (): Promise<RssRule[]> => get('/rss-rules'),
    save: (rule: RssRule): Promise<RssRule[]> => post('/rss-rules', rule),
    delete: (id: string): Promise<RssRule[]> => delJson(`/rss-rules/${encodeURIComponent(id)}`),
    test: (title: string, link = ''): Promise<{ matches: RssRuleMatch[] }> =>
      post('/rss-rules/test', { title, link: link || null }),
    apply: (title: string, link: string, dry_run = true): Promise<BulkResult> =>
      post('/rss-rules/apply', { title, link, dry_run }),
  },

  health: (): Promise<HealthResponse> =>
    getRoot('/health'),
}
