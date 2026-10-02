import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { cleanup, render, screen, waitFor, within } from '@testing-library/react'
import userEvent from '@testing-library/user-event'
import { QueryClient, QueryClientProvider } from '@tanstack/react-query'
import {
  CrashSafetyPanel,
  MAX_PATH_POLICIES,
  PRESETS,
  emptyPathPolicy,
  looksAbsolute,
  parsePathList,
  validateDraft,
} from './CrashSafetyPanel'
import type { CrashSafetyPathReport, CrashSafetySettings, CrashSafetyView } from '../api/client'

const DEFAULTS: CrashSafetySettings = {
  completion_gate: true,
  host_crash_detection: true,
  host_crash_recovery: 'recent',
  recent_write_window_secs: 24 * 3600,
  structural_audit: 'on_unclean',
  mount_probe: true,
  weak_mount_escalation: true,
  weak_mount_paths: [],
  strong_mount_paths: [],
  completion_verify: 'off',
  completion_verify_sample_percent: 5,
  path_policies: [],
}

function makeView(overrides: Partial<CrashSafetyView> = {}, verdict: CrashSafetyView['report']['previous_run']['verdict'] = 'clean'): CrashSafetyView {
  return {
    settings: DEFAULTS,
    defaults: DEFAULTS,
    overridden: false,
    report: {
      run_started_unix: 1_700_000_000,
      detection_active: true,
      previous_run: {
        verdict,
        crash_reference_unix: verdict === 'host_crash' ? 1_699_999_000 : null,
        previous_boot_id: null,
        current_boot_id: null,
      },
      counters: {
        recovery_rechecks: 3,
        rechecks_unsynced_state: 0,
        rechecks_full_policy: 0,
        rechecks_recent_write: 3,
        rechecks_weak_mount: 0,
        rechecks_unknown_mount: 0,
        audit_torrents: 5,
        audit_files_unsupported: 0,
        audit_pieces_downgraded: 12,
        completions_gated: 7,
        completions_released: 6,
        completion_gate_retries: 1,
        completion_sync_unsupported: 0,
        completion_verify_pieces: 40,
        completion_verify_failures: 0,
        integrity_regressions: 0,
        completions_pending: 1,
      },
      mounts: [{ path: '/data', trust: 'strong', fs_type: 'ext4', reasons: ['local block filesystem'] }],
      platform: { boot_identity: true, allocation_audit: true, mount_probe: true, page_cache_drop: true },
    },
    ...overrides,
  }
}

let view: CrashSafetyView
let putBodies: CrashSafetySettings[]
let resetCalls: number
let supported: boolean
let pathQueries: string[]
let pathReport: CrashSafetyPathReport | Response

const REPORT: CrashSafetyPathReport = {
  path: '/mnt/nas/tv',
  matched_policy: '/mnt/nas',
  completion_gate: true,
  host_crash_recovery: 'watermark',
  host_crash_recovery_effective: 'full',
  recovery_escalated_by: 'weak_mount',
  structural_audit: 'on_unclean',
  completion_verify: 'off',
  completion_verify_effective: 'sample',
  completion_verify_sample_percent: 10,
  mount: { path: '/mnt/nas/tv', trust: 'weak', fs_type: 'nfs4', reasons: ['network filesystem'] },
}

function installFetch() {
  putBodies = []
  resetCalls = 0
  pathQueries = []
  pathReport = REPORT
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = new URL(String(input), 'http://localhost')
    const json = (body: unknown, status = 200) =>
      new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
    if (url.pathname === '/api/v1/engine') {
      return json({ backend: { type: 'torrentng', capabilities: { supports_crash_safety: supported } } })
    }
    if (url.pathname === '/api/v1/settings/crash-safety/path') {
      pathQueries.push(url.searchParams.get('path') ?? '')
      return pathReport instanceof Response ? pathReport : json(pathReport)
    }
    if (url.pathname === '/api/v1/settings/crash-safety') {
      if (init?.method === 'PUT') {
        const body = JSON.parse(String(init.body)) as CrashSafetySettings
        putBodies.push(body)
        view = { ...view, settings: body, overridden: true }
        return json(view)
      }
      if (init?.method === 'DELETE') {
        resetCalls += 1
        view = { ...view, settings: view.defaults, overridden: false }
        return json(view)
      }
      return json(view)
    }
    return json({})
  }))
}

function renderPanel() {
  const client = new QueryClient({ defaultOptions: { queries: { retry: false } } })
  return render(
    <QueryClientProvider client={client}>
      <CrashSafetyPanel />
    </QueryClientProvider>,
  )
}

beforeEach(() => {
  supported = true
  view = makeView()
  installFetch()
})
afterEach(() => {
  cleanup()
  vi.unstubAllGlobals()
})

describe('crash-safety helpers', () => {
  it('parses one path per line, trimming blanks', () => {
    expect(parsePathList('  /mnt/a \n\n/mnt/b\n   \n')).toEqual(['/mnt/a', '/mnt/b'])
    expect(parsePathList('')).toEqual([])
  })

  it('recognizes absolute POSIX, drive and UNC paths only', () => {
    for (const ok of ['/srv', 'C:\\media', 'D:/media', '\\\\nas\\share']) expect(looksAbsolute(ok)).toBe(true)
    for (const bad of ['relative/path', './x', 'media', '']) expect(looksAbsolute(bad)).toBe(false)
  })

  it('accepts the defaults and rejects out-of-range or conflicting values', () => {
    expect(validateDraft(DEFAULTS)).toEqual([])
    expect(validateDraft({ ...DEFAULTS, recent_write_window_secs: 30 })).toHaveLength(1)
    expect(validateDraft({ ...DEFAULTS, recent_write_window_secs: 400 * 24 * 3600 })).toHaveLength(1)
    expect(validateDraft({ ...DEFAULTS, completion_verify_sample_percent: 0 })).toHaveLength(1)
    expect(validateDraft({ ...DEFAULTS, completion_verify_sample_percent: 101 })).toHaveLength(1)
    expect(validateDraft({ ...DEFAULTS, completion_verify_sample_percent: 2.5 })).toHaveLength(1)
    expect(validateDraft({ ...DEFAULTS, weak_mount_paths: ['nope'] })[0]).toMatch(/absolute/)
    expect(validateDraft({ ...DEFAULTS, weak_mount_paths: ['/a'], strong_mount_paths: ['/a'] })[0]).toMatch(/both/)
  })

  it('validates folder policies: absolute unique paths, whole-number sample sizes, a cap', () => {
    const at = (path: string, extra: Partial<ReturnType<typeof emptyPathPolicy>> = {}) => ({ ...emptyPathPolicy(), path, ...extra })
    expect(validateDraft({ ...DEFAULTS, path_policies: [at('/mnt/nas'), at('/scratch')] })).toEqual([])
    // A server that predates the feature omits the key entirely.
    expect(validateDraft({ ...DEFAULTS, path_policies: undefined })).toEqual([])

    expect(validateDraft({ ...DEFAULTS, path_policies: [at('')] })[0]).toMatch(/Location policy 1 needs an absolute/)
    expect(validateDraft({ ...DEFAULTS, path_policies: [at('relative/dir')] })[0]).toMatch(/absolute/)
    expect(validateDraft({ ...DEFAULTS, path_policies: [at('/a'), at(' /a ')] })[0]).toMatch(/only one policy/)
    for (const bad of [0, 101, 2.5]) {
      expect(
        validateDraft({ ...DEFAULTS, path_policies: [at('/a', { completion_verify_sample_percent: bad })] })[0],
        String(bad),
      ).toMatch(/sample size/)
    }
    expect(validateDraft({ ...DEFAULTS, path_policies: [at('/a', { completion_verify_sample_percent: 100 })] })).toEqual([])
    const many = Array.from({ length: MAX_PATH_POLICIES + 1 }, (_, i) => at(`/p${i}`))
    expect(validateDraft({ ...DEFAULTS, path_policies: many }).join(' ')).toMatch(/At most 64/)
  })

  it('every preset produces a valid configuration and keeps path overrides', () => {
    const policies = [{ ...emptyPathPolicy(), path: '/mnt/nas', completion_gate: false as const }]
    const base = { ...DEFAULTS, weak_mount_paths: ['/mnt/pool'], strong_mount_paths: ['/mnt/ssd'], path_policies: policies }
    for (const preset of PRESETS) {
      const applied = preset.apply(base)
      expect(validateDraft(applied), preset.id).toEqual([])
      expect(applied.weak_mount_paths).toEqual(['/mnt/pool'])
      expect(applied.strong_mount_paths).toEqual(['/mnt/ssd'])
      expect(applied.path_policies, preset.id).toEqual(policies)
      // No preset may switch the completion gate off.
      expect(applied.completion_gate, preset.id).toBe(true)
    }
    expect(PRESETS.find(p => p.id === 'balanced')?.apply({ ...DEFAULTS, completion_verify: 'full' })).toEqual(DEFAULTS)
  })
})

describe('CrashSafetyPanel', () => {
  it('explains itself and reports a clean previous shutdown', async () => {
    renderPanel()
    expect(await screen.findByRole('heading', { name: 'Crash safety' })).toBeTruthy()
    const status = await screen.findByTestId('crash-safety-status')
    expect(within(status).getByText('Clean shutdown')).toBeTruthy()
    expect(within(status).queryByTestId('crash-recovery-summary')).toBeNull()
    expect(within(status).getByText('Finishing now')).toBeTruthy()
    // Every control has an accessible name and a description.
    const gate = screen.getByLabelText('Hold completion until data is synced') as HTMLInputElement
    expect(gate.checked).toBe(true)
    expect(gate.getAttribute('aria-describedby')).toBeTruthy()
    expect(screen.getByLabelText('Recovery policy')).toBeTruthy()
    expect(screen.getByLabelText('Allocation audit')).toBeTruthy()
  })

  it('shows what recovery did after a power loss', async () => {
    view = makeView({}, 'host_crash')
    renderPanel()
    const summary = await screen.findByTestId('crash-recovery-summary')
    expect(summary.textContent).toContain('3 torrents rechecked')
    expect(summary.textContent).toContain('12 pieces re-verified')
    expect(screen.getByText('Power loss or system crash')).toBeTruthy()
  })

  it('warns loudly about integrity regressions', async () => {
    view = makeView()
    view.report.counters.integrity_regressions = 2
    renderPanel()
    const alert = await screen.findByText(/already reported complete/)
    expect(alert.closest('[role="alert"]')).toBeTruthy()
  })

  it('applies edited settings and reflects the override', async () => {
    const user = userEvent.setup()
    renderPanel()
    const apply = await screen.findByRole('button', { name: 'Apply' }) as HTMLButtonElement
    expect(apply.disabled).toBe(true)

    await user.selectOptions(screen.getByLabelText('Read-back verification'), 'sample')
    await user.selectOptions(screen.getByLabelText('Recovery policy'), 'full')
    expect(screen.getByText('Unsaved')).toBeTruthy()
    expect(apply.disabled).toBe(false)
    await user.click(apply)

    await waitFor(() => expect(putBodies).toHaveLength(1))
    expect(putBodies[0].completion_verify).toBe('sample')
    expect(putBodies[0].host_crash_recovery).toBe('full')
    expect(await screen.findByText('Applied')).toBeTruthy()
    expect(await screen.findByRole('button', { name: 'Reset to config file' })).toBeTruthy()
    expect(screen.getByText(/changed at runtime and override the config file/)).toBeTruthy()
  })

  it('discards edits and resets the runtime override', async () => {
    view = makeView({ overridden: true, settings: { ...DEFAULTS, completion_verify: 'full' } })
    const user = userEvent.setup()
    renderPanel()
    const verify = await screen.findByLabelText('Read-back verification') as HTMLSelectElement
    await waitFor(() => expect(verify.value).toBe('full'))

    await user.selectOptions(verify, 'off')
    await user.click(screen.getByRole('button', { name: 'Discard changes' }))
    expect(verify.value).toBe('full')

    await user.click(screen.getByRole('button', { name: 'Reset to config file' }))
    await waitFor(() => expect(resetCalls).toBe(1))
    await waitFor(() => expect(verify.value).toBe('off'))
    expect(screen.queryByRole('button', { name: 'Reset to config file' })).toBeNull()
  })

  it('blocks Apply and explains why when a value is invalid', async () => {
    const user = userEvent.setup()
    renderPanel()
    await screen.findByLabelText('Always treat as weak')
    await user.type(screen.getByLabelText('Always treat as weak'), 'not-absolute')
    const alert = await screen.findByRole('alert')
    expect(alert.textContent).toMatch(/must be absolute/)
    expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(true)
    expect(putBodies).toHaveLength(0)
  })

  it('applies a preset to the draft without saving it', async () => {
    const user = userEvent.setup()
    renderPanel()
    await screen.findByRole('group', { name: 'Crash-safety presets' })
    await user.click(screen.getByRole('button', { name: 'Maximum safety' }))
    expect((screen.getByLabelText('Read-back verification') as HTMLSelectElement).value).toBe('full')
    expect((screen.getByLabelText('Recovery policy') as HTMLSelectElement).value).toBe('full')
    expect(putBodies).toHaveLength(0)
    expect(screen.getByText('Unsaved')).toBeTruthy()
  })

  it('disables options the platform cannot honor and says so', async () => {
    view = makeView()
    view.report.platform = { boot_identity: false, allocation_audit: false, mount_probe: false, page_cache_drop: false }
    renderPanel()
    const audit = await screen.findByLabelText('Allocation audit') as HTMLSelectElement
    expect(audit.disabled).toBe(true)
    expect(screen.getByText(/Not available on this operating system, so this option has no effect here/)).toBeTruthy()
    expect((screen.getByLabelText('Classify each save location') as HTMLInputElement).disabled).toBe(true)
    expect(screen.getByText(/cannot report a boot identity/)).toBeTruthy()
  })

  it('lists rated save locations with their reasons', async () => {
    view = makeView()
    view.report.mounts = [
      { path: '/mnt/tmp', trust: 'weak', fs_type: 'tmpfs', reasons: ['`tmpfs` is volatile'] },
      { path: '/mnt/nas', trust: 'unknown', fs_type: 'nfs4', reasons: ['network filesystem'] },
    ]
    renderPanel()
    const table = await screen.findByRole('table')
    expect(within(table).getByText('Weak')).toBeTruthy()
    expect(within(table).getByText('Unknown')).toBeTruthy()
    expect(within(table).getByText(/volatile/)).toBeTruthy()
  })

  it('adds a folder policy, leaves unset options inheriting, and sends nulls for them', async () => {
    const user = userEvent.setup()
    renderPanel()
    expect(await screen.findByTestId('no-path-policies')).toBeTruthy()
    await user.click(screen.getByRole('button', { name: 'Add folder policy' }))

    const policy = await screen.findByTestId('path-policy')
    // A blank folder is refused with an explanation, and Apply stays disabled.
    expect((await screen.findByRole('alert')).textContent).toMatch(/Location policy 1 needs an absolute folder path/)
    expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(true)

    await user.type(within(policy).getByLabelText('Folder'), '/mnt/nas')
    await user.selectOptions(within(policy).getByLabelText('Read-back verification'), 'sample')
    await user.type(within(policy).getByLabelText('Sample size (percent)'), '10')
    await user.selectOptions(within(policy).getByLabelText('Hold completion until synced'), 'off')
    expect(screen.getByText('Unsaved')).toBeTruthy()

    await user.click(screen.getByRole('button', { name: 'Apply' }))
    await waitFor(() => expect(putBodies).toHaveLength(1))
    expect(putBodies[0].path_policies).toEqual([{
      path: '/mnt/nas',
      completion_gate: false,
      host_crash_recovery: null,
      structural_audit: null,
      completion_verify: 'sample',
      completion_verify_sample_percent: 10,
    }])
    expect(await screen.findByText('Applied')).toBeTruthy()
    // The saved state is adopted: nothing left unsaved.
    await waitFor(() => expect(screen.queryByText('Unsaved')).toBeNull())
  })

  it('shows saved folder policies and removes one', async () => {
    view = makeView({
      settings: {
        ...DEFAULTS,
        path_policies: [
          { ...emptyPathPolicy(), path: '/mnt/nas', host_crash_recovery: 'full' },
          { ...emptyPathPolicy(), path: '/scratch', completion_gate: false },
        ],
      },
    })
    const user = userEvent.setup()
    renderPanel()
    const policies = await screen.findAllByTestId('path-policy')
    expect(policies).toHaveLength(2)
    expect((within(policies[0]).getByLabelText('Folder') as HTMLInputElement).value).toBe('/mnt/nas')
    expect((within(policies[0]).getByLabelText('Recovery policy') as HTMLSelectElement).value).toBe('full')
    // Unset options read "Use global setting".
    expect((within(policies[0]).getByLabelText('Allocation audit') as HTMLSelectElement).value).toBe('')
    expect((within(policies[1]).getByLabelText('Hold completion until synced') as HTMLSelectElement).value).toBe('off')

    await user.click(screen.getByRole('button', { name: 'Remove folder policy 1 for /mnt/nas' }))
    expect(screen.getAllByTestId('path-policy')).toHaveLength(1)
    await user.click(screen.getByRole('button', { name: 'Apply' }))
    await waitFor(() => expect(putBodies).toHaveLength(1))
    expect(putBodies[0].path_policies?.map(policy => policy.path)).toEqual(['/scratch'])
  })

  it('treats a server without path_policies as having none and is not dirty', async () => {
    const { path_policies: _omitted, ...legacy } = DEFAULTS
    void _omitted
    view = makeView({ settings: legacy, defaults: legacy })
    renderPanel()
    expect(await screen.findByTestId('no-path-policies')).toBeTruthy()
    expect(screen.queryByText('Unsaved')).toBeNull()
    expect((screen.getByRole('button', { name: 'Apply' }) as HTMLButtonElement).disabled).toBe(true)
  })

  it('checks a location and explains what escalation did', async () => {
    const user = userEvent.setup()
    renderPanel()
    const form = await screen.findByRole('form', { name: 'Check a location' })
    const check = within(form).getByRole('button', { name: 'Check' }) as HTMLButtonElement
    expect(check.disabled).toBe(true)

    await user.type(within(form).getByLabelText('Folder'), 'relative')
    expect(within(form).getByText('Enter an absolute folder path.')).toBeTruthy()
    expect(check.disabled).toBe(true)

    await user.clear(within(form).getByLabelText('Folder'))
    await user.type(within(form).getByLabelText('Folder'), '/mnt/nas/tv')
    await user.click(check)

    const report = await screen.findByTestId('path-report')
    expect(pathQueries).toEqual(['/mnt/nas/tv'])
    expect(within(report).getByText('/mnt/nas')).toBeTruthy()
    expect(within(report).getByText('Weak')).toBeTruthy()
    expect(report.textContent).toContain('Raised from Watermark only because this location is rated Weak')
    expect(report.textContent).toContain('Sample (10%)')
    expect(report.textContent).toContain('Raised from Off because this location is rated Weak')
  })

  it('says so when no folder policy matches and reports a failed check', async () => {
    const user = userEvent.setup()
    pathReport = {
      ...REPORT,
      matched_policy: null,
      host_crash_recovery: 'recent',
      host_crash_recovery_effective: 'recent',
      recovery_escalated_by: null,
      completion_verify_effective: 'off',
      mount: { path: '/data', trust: 'strong', fs_type: 'ext4', reasons: [] },
    }
    renderPanel()
    const form = await screen.findByRole('form', { name: 'Check a location' })
    await user.type(within(form).getByLabelText('Folder'), '/data')
    await user.click(within(form).getByRole('button', { name: 'Check' }))
    const report = await screen.findByTestId('path-report')
    expect(report.textContent).toContain('None. The global settings apply.')
    expect(report.textContent).not.toContain('Raised from')

    pathReport = new Response('bad', { status: 400 })
    await user.click(within(form).getByRole('button', { name: 'Check' }))
    expect((await within(form).findByRole('alert')).textContent).toMatch(/Could not check that folder: API 400/)
  })

  it('discards an answer that described settings that have since changed', async () => {
    const user = userEvent.setup()
    renderPanel()
    const form = await screen.findByRole('form', { name: 'Check a location' })
    await user.type(within(form).getByLabelText('Folder'), '/mnt/nas/tv')
    await user.click(within(form).getByRole('button', { name: 'Check' }))
    await screen.findByTestId('path-report')

    await user.selectOptions(screen.getByLabelText('Recovery policy'), 'full')
    await user.click(screen.getByRole('button', { name: 'Apply' }))
    await waitFor(() => expect(screen.queryByTestId('path-report')).toBeNull())
  })

  it('is replaced by an explanation on backends without native crash safety', async () => {
    supported = false
    renderPanel()
    expect(await screen.findByText(/available when TorrentNG runs its own client/)).toBeTruthy()
    expect(screen.queryByLabelText('Recovery policy')).toBeNull()
  })

  it('reports a load failure instead of an empty form', async () => {
    vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo | URL) => {
      const url = new URL(String(input), 'http://localhost')
      if (url.pathname === '/api/v1/engine') {
        return new Response(JSON.stringify({ backend: { type: 'torrentng', capabilities: { supports_crash_safety: true } } }), {
          status: 200, headers: { 'content-type': 'application/json' },
        })
      }
      return new Response('nope', { status: 503 })
    }))
    renderPanel()
    expect((await screen.findByRole('alert')).textContent).toMatch(/Could not load crash-safety settings/)
  })
})
