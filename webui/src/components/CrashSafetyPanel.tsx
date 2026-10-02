import { useEffect, useId, useMemo, useRef, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import {
  api,
  type CompletionVerifyMode,
  type CrashSafetyPathPolicy,
  type CrashSafetyPathReport,
  type CrashSafetySettings,
  type CrashSafetyView,
  type DurabilityTrust,
  type HostCrashRecovery,
  type PreviousRunVerdict,
  type StructuralAuditMode,
} from '../api/client'

const HOUR = 3600
/** Mirrors the daemon's limit on `path_policies`. */
export const MAX_PATH_POLICIES = 64

const RECOVERY_LABEL: Record<HostCrashRecovery, string> = {
  watermark: 'Watermark only',
  recent: 'Recent writes',
  full: 'Full recheck',
}
const AUDIT_LABEL: Record<StructuralAuditMode, string> = {
  off: 'Off',
  on_unclean: 'After an unclean shutdown',
  always: 'Every start',
}
const VERIFY_LABEL: Record<CompletionVerifyMode, string> = {
  off: 'Off',
  sample: 'Sample',
  full: 'Full',
}
const ESCALATION_TEXT: Record<string, string> = {
  weak_mount: 'this location is rated Weak',
  unknown_mount_recent_write: 'this location is rated Unknown',
}

export function emptyPathPolicy(): CrashSafetyPathPolicy {
  return {
    path: '',
    completion_gate: null,
    host_crash_recovery: null,
    structural_audit: null,
    completion_verify: null,
    completion_verify_sample_percent: null,
  }
}

/** Older servers omit `path_policies`; treat that as none. */
function withPolicies(settings: CrashSafetySettings): CrashSafetySettings {
  return { ...settings, path_policies: settings.path_policies ?? [] }
}

type Preset = { id: string; label: string; blurb: string; apply: (base: CrashSafetySettings) => CrashSafetySettings }

/**
 * Presets change only the behavior knobs. The operator's path overrides are
 * never touched by a preset.
 */
export const PRESETS: Preset[] = [
  {
    id: 'balanced',
    label: 'Balanced (default)',
    blurb: 'Hold completions until synced; recheck recently written torrents after a power loss.',
    apply: base => ({
      ...base,
      completion_gate: true,
      host_crash_detection: true,
      host_crash_recovery: 'recent',
      recent_write_window_secs: 24 * HOUR,
      structural_audit: 'on_unclean',
      mount_probe: true,
      weak_mount_escalation: true,
      completion_verify: 'off',
      completion_verify_sample_percent: 5,
    }),
  },
  {
    id: 'maximum',
    label: 'Maximum safety',
    blurb: 'Re-read every finished download; recheck everything after a power loss. Slow on big libraries.',
    apply: base => ({
      ...base,
      completion_gate: true,
      host_crash_detection: true,
      host_crash_recovery: 'full',
      structural_audit: 'always',
      mount_probe: true,
      weak_mount_escalation: true,
      completion_verify: 'full',
    }),
  },
  {
    id: 'lean',
    label: 'Lean',
    blurb: 'Least extra work: trust saved state after a power loss and recheck only the unsynced tail.',
    apply: base => ({
      ...base,
      completion_gate: true,
      host_crash_detection: true,
      host_crash_recovery: 'watermark',
      structural_audit: 'off',
      weak_mount_escalation: false,
      completion_verify: 'off',
    }),
  },
]

export function parsePathList(text: string): string[] {
  return text
    .split('\n')
    .map(line => line.trim())
    .filter(Boolean)
}

export function looksAbsolute(path: string): boolean {
  return path.startsWith('/') || /^[A-Za-z]:[\\/]/.test(path) || path.startsWith('\\\\')
}

export function validateDraft(draft: CrashSafetySettings): string[] {
  const problems: string[] = []
  if (!Number.isFinite(draft.recent_write_window_secs) || draft.recent_write_window_secs < 60) {
    problems.push('The recent-write window must be at least one minute.')
  }
  if (draft.recent_write_window_secs > 365 * 24 * HOUR) {
    problems.push('The recent-write window cannot exceed 365 days.')
  }
  if (
    !Number.isInteger(draft.completion_verify_sample_percent)
    || draft.completion_verify_sample_percent < 1
    || draft.completion_verify_sample_percent > 100
  ) {
    problems.push('The sample size must be a whole number from 1 to 100 percent.')
  }
  for (const [name, paths] of [['Weak', draft.weak_mount_paths], ['Strong', draft.strong_mount_paths]] as const) {
    const bad = paths.find(path => !looksAbsolute(path))
    if (bad) problems.push(`${name} mount paths must be absolute: “${bad}”.`)
  }
  const overlap = draft.weak_mount_paths.find(path => draft.strong_mount_paths.includes(path))
  if (overlap) problems.push(`“${overlap}” is listed as both weak and strong.`)
  const policies = draft.path_policies ?? []
  if (policies.length > MAX_PATH_POLICIES) {
    problems.push(`At most ${MAX_PATH_POLICIES} location policies are allowed.`)
  }
  policies.forEach((policy, index) => {
    const path = policy.path.trim()
    const name = path ? `“${path}”` : `Location policy ${index + 1}`
    if (!looksAbsolute(path)) problems.push(`${name} needs an absolute folder path.`)
    const percent = policy.completion_verify_sample_percent
    if (percent !== null && (!Number.isInteger(percent) || percent < 1 || percent > 100)) {
      problems.push(`${name}: the sample size must be a whole number from 1 to 100 percent.`)
    }
    if (path && policies.findIndex(other => other.path.trim() === path) !== index) {
      problems.push(`${name} appears more than once; each location can have only one policy.`)
    }
  })
  return problems
}

const VERDICT_TEXT: Record<PreviousRunVerdict, { title: string; tone: 'ok' | 'info' | 'warn'; detail: string }> = {
  no_record: {
    title: 'No record',
    tone: 'info',
    detail: 'This is the first start, or the previous version did not track how it shut down.',
  },
  clean: {
    title: 'Clean shutdown',
    tone: 'ok',
    detail: 'The previous run shut down cleanly. Saved state is trusted as usual.',
  },
  process_crash: {
    title: 'Process crash',
    tone: 'warn',
    detail:
      'The previous run ended abruptly, but the machine stayed up, so data already written to disk is intact. Only the most recent unsynced pieces are rechecked.',
  },
  host_crash: {
    title: 'Power loss or system crash',
    tone: 'warn',
    detail:
      'The machine restarted since the previous run ended. Data that was never synced to disk may be missing or zero-filled, so recovery checks were applied.',
  },
  unclean_unknown_cause: {
    title: 'Unclean shutdown (cause unknown)',
    tone: 'warn',
    detail:
      'The previous run ended abruptly and this operating system cannot tell whether the machine restarted, so it is handled as a possible power loss.',
  },
}

const TRUST_TEXT: Record<DurabilityTrust, { label: string; tone: 'ok' | 'info' | 'warn' }> = {
  strong: { label: 'Strong', tone: 'ok' },
  unknown: { label: 'Unknown', tone: 'info' },
  weak: { label: 'Weak', tone: 'warn' },
}

function sameSettings(a: CrashSafetySettings, b: CrashSafetySettings): boolean {
  return JSON.stringify(a) === JSON.stringify(b)
}

function formatWhen(unix: number | null): string {
  if (!unix) return 'unknown'
  return new Date(unix * 1000).toLocaleString()
}

export function CrashSafetyPanel() {
  const qc = useQueryClient()
  const { data: engine } = useQuery({ queryKey: ['engine'], queryFn: api.engine, staleTime: 5_000 })
  const supported = engine?.backend.capabilities.supports_crash_safety === true
  const { data, isLoading, error } = useQuery({
    queryKey: ['crash-safety'],
    queryFn: api.settings.getCrashSafety,
    enabled: supported,
    refetchInterval: 10_000,
  })
  const view = useMemo<CrashSafetyView | undefined>(
    () => (data && data.settings && data.report
      ? { ...data, settings: withPolicies(data.settings), defaults: withPolicies(data.defaults ?? data.settings) }
      : undefined),
    [data],
  )

  const [draft, setDraft] = useState<CrashSafetySettings | null>(null)
  const [weakText, setWeakText] = useState('')
  const [strongText, setStrongText] = useState('')
  const [saved, setSaved] = useState<'applied' | 'reset' | null>(null)
  const [checkPath, setCheckPath] = useState('')
  const ids = { title: useId(), status: useId(), form: useId() }

  // Adopt server state, but never overwrite edits the operator is making: a
  // field follows the server only while it still equals the last server value.
  const baseline = useRef<CrashSafetySettings | null>(null)
  const serverSettings = view?.settings
  useEffect(() => {
    if (!serverSettings) return
    const previous = baseline.current
    baseline.current = serverSettings
    setDraft(current => (!current || (previous && sameSettings(current, previous)) ? serverSettings : current))
    setWeakText(text => (previous === null || text === previous.weak_mount_paths.join('\n')
      ? serverSettings.weak_mount_paths.join('\n')
      : text))
    setStrongText(text => (previous === null || text === previous.strong_mount_paths.join('\n')
      ? serverSettings.strong_mount_paths.join('\n')
      : text))
  }, [serverSettings])

  const effectiveDraft: CrashSafetySettings | null = draft && {
    ...draft,
    weak_mount_paths: parsePathList(weakText),
    strong_mount_paths: parsePathList(strongText),
    path_policies: (draft.path_policies ?? []).map(policy => ({ ...policy, path: policy.path.trim() })),
  }
  const problems = effectiveDraft ? validateDraft(effectiveDraft) : []
  const dirty = Boolean(view && effectiveDraft && !sameSettings(effectiveDraft, view.settings))

  const check = useMutation({
    mutationFn: (path: string) => api.settings.describeCrashSafetyPath(path),
  })
  const finish = (result: CrashSafetyView, kind: 'applied' | 'reset') => {
    baseline.current = result.settings
    qc.setQueryData(['crash-safety'], result)
    setDraft(result.settings)
    setWeakText(result.settings.weak_mount_paths.join('\n'))
    setStrongText(result.settings.strong_mount_paths.join('\n'))
    check.reset() // a previous answer described the old settings
    setSaved(kind)
    setTimeout(() => setSaved(null), 2500)
  }
  const apply = useMutation({
    mutationFn: (settings: CrashSafetySettings) => api.settings.putCrashSafety(settings),
    onSuccess: result => finish(result, 'applied'),
  })
  const reset = useMutation({
    mutationFn: () => api.settings.resetCrashSafety(),
    onSuccess: result => finish(result, 'reset'),
  })
  const busy = apply.isPending || reset.isPending

  if (!supported) {
    return (
      <section aria-labelledby={ids.title} style={sectionStyle}>
        <h2 id={ids.title} style={h2Style}>Crash safety</h2>
        <div role="status" style={infoStyle}>
          Crash-safety controls are available when TorrentNG runs its own client (<code>torrentngd</code>). The
          selected backend manages its own resume state, so a power loss is handled by that client&rsquo;s
          own recovery.
        </div>
      </section>
    )
  }

  if (!view || !draft || !effectiveDraft) {
    return (
      <section aria-labelledby={ids.title} aria-busy={isLoading} style={sectionStyle}>
        <h2 id={ids.title} style={h2Style}>Crash safety</h2>
        {error
          ? <div role="alert" style={errorStyle}>Could not load crash-safety settings: {error instanceof Error ? error.message : 'unknown error'}</div>
          : <div role="status" style={subtleStyle}>Loading…</div>}
      </section>
    )
  }

  const { report } = view
  const verdict = VERDICT_TEXT[report.previous_run.verdict]
  const unclean = ['process_crash', 'host_crash', 'unclean_unknown_cause'].includes(report.previous_run.verdict)
  const c = report.counters
  const set = <K extends keyof CrashSafetySettings>(key: K, value: CrashSafetySettings[K]) =>
    setDraft(current => (current ? { ...current, [key]: value } : current))
  const hours = Math.round(effectiveDraft.recent_write_window_secs / HOUR)
  const policies = draft.path_policies ?? []
  const setPolicy = (index: number, next: CrashSafetyPathPolicy) =>
    set('path_policies', policies.map((policy, i) => (i === index ? next : policy)))

  return (
    <section aria-labelledby={ids.title} aria-busy={busy} style={sectionStyle}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 6 }}>
        <div>
          <h2 id={ids.title} style={h2Style}>Crash safety</h2>
          <div style={subtleStyle}>
            Keeps a power cut or crash from marking half-written files as complete.
            {' '}
            <span title="docs/CRASH_SAFETY.md in the project repository">Full guide: <code>docs/CRASH_SAFETY.md</code></span>
          </div>
        </div>
        {dirty && <Pill tone="warn">Unsaved</Pill>}
      </div>

      {/* ---------- status ---------- */}
      <div className="tng-card" role="region" aria-labelledby={ids.status} style={cardStyle} data-testid="crash-safety-status">
        <div id={ids.status} style={cardTitleStyle}>Last shutdown</div>
        <div style={{ display: 'flex', gap: 8, alignItems: 'center', flexWrap: 'wrap' }}>
          <Pill tone={verdict.tone}>{verdict.title}</Pill>
          {report.previous_run.crash_reference_unix && unclean && (
            <span style={subtleStyle}>around {formatWhen(report.previous_run.crash_reference_unix)}</span>
          )}
        </div>
        <p style={paraStyle}>{verdict.detail}</p>
        {unclean && (
          <p style={paraStyle} data-testid="crash-recovery-summary">
            Recovery this run: <strong>{c.recovery_rechecks}</strong> torrent{c.recovery_rechecks === 1 ? '' : 's'} rechecked,{' '}
            <strong>{c.audit_pieces_downgraded}</strong> piece{c.audit_pieces_downgraded === 1 ? '' : 's'} re-verified by the
            allocation audit, <strong>{c.integrity_regressions}</strong> integrity regression{c.integrity_regressions === 1 ? '' : 's'}.
          </p>
        )}
        {c.integrity_regressions > 0 && (
          <div role="alert" style={warnStyle}>
            {c.integrity_regressions} torrent{c.integrity_regressions === 1 ? ' that was' : 's that were'} already reported complete
            had pieces that no longer verified. Missing data is being downloaded again in place, which also repairs
            hardlinked copies. Check the Logs panel for the affected files.
          </div>
        )}
        <dl style={statGridStyle}>
          <Stat label="Finishing now" value={c.completions_pending}
            hint="Downloads held at “Finalizing” until storage confirms the data is on disk." />
          <Stat label="Held this run" value={c.completions_gated} hint="Downloads that went through the completion gate since start." />
          <Stat label="Read-back checked" value={c.completion_verify_pieces} hint="Pieces re-read from disk after completing." />
          <Stat label="Read-back failures" value={c.completion_verify_failures} hint="Pieces that did not match when re-read." />
        </dl>
        {c.completion_sync_unsupported > 0 && (
          <div role="note" style={warnStyle}>
            A filesystem answered that <code>fsync</code> is not supported. Completions there are released without a data
            sync, and the mount is treated as weak.
          </div>
        )}
      </div>

      {/* ---------- presets ---------- */}
      <div style={{ marginTop: 14 }}>
        <div style={cardTitleStyle}>Presets</div>
        <div style={{ display: 'flex', flexWrap: 'wrap', gap: 6 }} role="group" aria-label="Crash-safety presets">
          {PRESETS.map(preset => (
            <button
              key={preset.id}
              type="button"
              className="tng-preset-button"
              title={preset.blurb}
              disabled={busy}
              style={presetButtonStyle}
              onClick={() => setDraft(preset.apply(effectiveDraft))}
            >
              {preset.label}
            </button>
          ))}
        </div>
        <div style={{ ...subtleStyle, marginTop: 6 }}>
          Presets change the options below without saving. Your mount-path overrides are never touched.
        </div>
      </div>

      {/* ---------- settings ---------- */}
      <form
        id={ids.form}
        style={{ marginTop: 14, display: 'grid', gap: 12 }}
        onSubmit={event => {
          event.preventDefault()
          if (problems.length === 0 && dirty) apply.mutate(effectiveDraft)
        }}
      >
        <Group title="When a download finishes">
          <Toggle
            label="Hold completion until data is synced"
            checked={effectiveDraft.completion_gate}
            onChange={value => set('completion_gate', value)}
            help="A finished download stays at “Finalizing” (one byte left) until its files are synced to disk. Sonarr, Radarr and trackers only see it as complete after that, so a power cut right after completion cannot hand them a half-written file."
            cost="Cost: one disk sync per completed torrent, usually well under a second."
          />
          <Select<CompletionVerifyMode>
            label="Read-back verification"
            value={effectiveDraft.completion_verify}
            onChange={value => set('completion_verify', value)}
            options={[
              ['off', 'Off'],
              ['sample', 'Sample'],
              ['full', 'Full'],
            ]}
            help="After the sync, drop cached pages where the OS allows it, re-read the files from disk and check their hashes before releasing. Sample checks the first and last piece of every file plus a percentage of the rest; Full checks everything. Catches storage that acknowledges writes it did not perform."
            cost="Cost: Sample re-reads a few percent; Full doubles the read I/O of a download."
          />
          <NumberField
            label="Sample size (percent of pieces)"
            value={effectiveDraft.completion_verify_sample_percent}
            min={1}
            max={100}
            disabled={effectiveDraft.completion_verify !== 'sample'}
            onChange={value => set('completion_verify_sample_percent', value)}
            help="Used only when read-back verification is set to Sample. File first and last pieces are always included."
          />
        </Group>

        <Group title="After a power loss or system crash">
          <Toggle
            label="Detect host crashes"
            checked={effectiveDraft.host_crash_detection}
            onChange={value => set('host_crash_detection', value)}
            help="Remember whether the last run ended cleanly and whether the machine restarted since, so a power loss can be told apart from a process crash. Required for the options below."
            cost={report.platform.boot_identity
              ? 'Cost: a tiny file in the session folder, rewritten once a minute.'
              : 'This operating system cannot report a boot identity, so every unclean restart is handled as a possible power loss.'}
          />
          <Select<HostCrashRecovery>
            label="Recovery policy"
            value={effectiveDraft.host_crash_recovery}
            onChange={value => set('host_crash_recovery', value)}
            disabled={!effectiveDraft.host_crash_detection}
            options={[
              ['watermark', 'Watermark only'],
              ['recent', 'Recent writes'],
              ['full', 'Full recheck'],
            ]}
            help="Watermark trusts saved state and rechecks only pieces written since the last sync. Recent writes also fully rechecks any torrent written inside the window below. Full recheck re-hashes every torrent."
            cost="Cost: Full recheck reads the whole library and is slow at scale."
          />
          <NumberField
            label="Recent-write window (hours)"
            value={hours}
            min={1}
            max={8760}
            disabled={!effectiveDraft.host_crash_detection || effectiveDraft.host_crash_recovery !== 'recent'}
            onChange={value => set('recent_write_window_secs', Math.max(1, Math.round(value)) * HOUR)}
            help="How far before the crash a torrent's last write may be for it to be rechecked. Used only with the Recent writes policy."
          />
          <Select<StructuralAuditMode>
            label="Allocation audit"
            value={effectiveDraft.structural_audit}
            onChange={value => set('structural_audit', value)}
            disabled={!report.platform.allocation_audit}
            options={[
              ['off', 'Off'],
              ['on_unclean', 'After an unclean shutdown'],
              ['always', 'Every start'],
            ]}
            help="Ask the filesystem which parts of each file were never written (holes, or preallocated space with no data) and re-verify any piece that claims to be there. Reads metadata only, so it is fast. It only triggers re-hashing; it never declares a piece corrupt by itself."
            cost={report.platform.allocation_audit
              ? 'Cost: one allocation query per file at start.'
              : 'Not available on this operating system, so this option has no effect here.'}
          />
        </Group>

        <Group title="Storage trust">
          <Toggle
            label="Classify each save location"
            checked={effectiveDraft.mount_probe}
            onChange={value => set('mount_probe', value)}
            disabled={!report.platform.mount_probe}
            help="Look at the filesystem and mount options under each save path and rate how far its fsync can be trusted. Volatile filesystems and mounts with write barriers disabled are rated Weak; network, FUSE and overlay mounts are rated Unknown."
            cost={report.platform.mount_probe ? undefined : 'Not available on this operating system.'}
          />
          <Toggle
            label="Check harder on weak or unknown storage"
            checked={effectiveDraft.weak_mount_escalation}
            onChange={value => set('weak_mount_escalation', value)}
            help="After a power loss, Weak mounts are fully rechecked and Unknown mounts get at least the recent-writes check. Weak mounts also get a sampled read-back after every download."
          />
          <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(240px, 1fr))' }}>
            <PathList
              label="Always treat as weak"
              value={weakText}
              onChange={setWeakText}
              help="One absolute path per line. Use for storage the probe cannot see through, such as a ZFS dataset with sync disabled."
            />
            <PathList
              label="Always treat as strong"
              value={strongText}
              onChange={setStrongText}
              help="One absolute path per line. Use when you know a network or FUSE mount honors fsync."
            />
          </div>
        </Group>

        <Group title="Different settings for specific folders">
          <div style={helpStyle}>
            One library can sit on different storage. Add a policy for a folder to give torrents saved there their own
            completion, recovery, audit or read-back settings. The most specific folder wins, and any option you leave
            on “Use global setting” follows the settings above. Storage trust still applies on top: a weak location is
            checked harder than its policy alone would ask.
          </div>
          {policies.length === 0
            ? <div style={subtleStyle} data-testid="no-path-policies">No folder policies. Every save location uses the settings above.</div>
            : policies.map((policy, index) => (
              <PolicyEditor
                key={index}
                index={index}
                policy={policy}
                onChange={next => setPolicy(index, next)}
                onRemove={() => set('path_policies', policies.filter((_, i) => i !== index))}
              />
            ))}
          <div>
            <button
              type="button"
              style={secondaryButtonStyle}
              disabled={busy || policies.length >= MAX_PATH_POLICIES}
              onClick={() => set('path_policies', [...policies, emptyPathPolicy()])}
            >
              Add folder policy
            </button>
          </div>
        </Group>

        {problems.length > 0 && (
          <ul role="alert" style={{ ...errorStyle, margin: 0, paddingLeft: 22 }}>
            {problems.map(problem => <li key={problem}>{problem}</li>)}
          </ul>
        )}
        {(apply.isError || reset.isError) && (
          <div role="alert" style={errorStyle}>
            {((apply.error ?? reset.error) instanceof Error ? (apply.error ?? reset.error)?.message : null) ?? 'The change was not applied.'}
          </div>
        )}

        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center' }}>
          <button type="submit" disabled={!dirty || problems.length > 0 || busy} style={primaryButtonStyle(dirty && problems.length === 0 && !busy)}>
            {apply.isPending ? 'Applying…' : 'Apply'}
          </button>
          <button
            type="button"
            disabled={!dirty || busy}
            style={secondaryButtonStyle}
            onClick={() => {
              setDraft(view.settings)
              setWeakText(view.settings.weak_mount_paths.join('\n'))
              setStrongText(view.settings.strong_mount_paths.join('\n'))
            }}
          >
            Discard changes
          </button>
          {view.overridden && (
            <button type="button" disabled={busy} style={secondaryButtonStyle} onClick={() => reset.mutate()}
              title="Forget the runtime change and use the values from the config file again">
              Reset to config file
            </button>
          )}
          <span role="status" aria-live="polite" style={{ minHeight: 18 }}>
            {saved === 'applied' && <Pill tone="ok">Applied</Pill>}
            {saved === 'reset' && <Pill tone="ok">Reset</Pill>}
          </span>
        </div>
        <div style={subtleStyle}>
          {view.overridden
            ? 'These settings were changed at runtime and override the config file. They persist across restarts until reset.'
            : 'These settings come from the config file. Changes made here are saved separately and persist across restarts.'}
          {' '}Changes take effect immediately, for torrents that start or finish from now on.
        </div>
      </form>

      {/* ---------- check a location ---------- */}
      <form
        style={{ marginTop: 16 }}
        aria-label="Check a location"
        onSubmit={event => {
          event.preventDefault()
          const path = checkPath.trim()
          if (looksAbsolute(path)) check.mutate(path)
        }}
      >
        <div style={cardTitleStyle}>Check a location</div>
        <div style={{ ...subtleStyle, marginBottom: 6 }}>
          See which settings a folder gets right now, how its storage is rated and what recovery would do there. The
          folder does not need to exist and is not added to the list below.
        </div>
        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'flex-end' }}>
          <div style={{ flex: '1 1 260px' }}>
            <label htmlFor={`${ids.form}-check`} style={fieldLabelStyle}>Folder</label>
            <input
              id={`${ids.form}-check`}
              type="text"
              value={checkPath}
              placeholder="/mnt/nas/tv"
              spellCheck={false}
              style={{ ...inputStyle, fontFamily: 'monospace' }}
              onChange={event => setCheckPath(event.target.value)}
            />
          </div>
          <button type="submit" style={secondaryButtonStyle} disabled={!looksAbsolute(checkPath.trim()) || check.isPending}>
            {check.isPending ? 'Checking…' : 'Check'}
          </button>
        </div>
        {checkPath.trim() !== '' && !looksAbsolute(checkPath.trim()) && (
          <div style={{ ...subtleStyle, color: 'var(--warning)' }}>Enter an absolute folder path.</div>
        )}
        <div role="status" aria-live="polite">
          {check.isError && (
            <div role="alert" style={{ ...errorStyle, marginTop: 8 }}>
              Could not check that folder: {check.error instanceof Error ? check.error.message : 'unknown error'}
            </div>
          )}
          {check.data && <PathReportView report={check.data} />}
        </div>
      </form>

      {/* ---------- mounts ---------- */}
      <div style={{ marginTop: 16 }}>
        <div style={cardTitleStyle}>Save locations seen so far</div>
        {report.mounts.length === 0
          ? <div style={subtleStyle}>None yet. A location appears here once a torrent using it starts.</div>
          : (
            <table style={tableStyle}>
              <caption style={visuallyHidden}>Durability rating of each save location</caption>
              <thead>
                <tr>
                  <th scope="col" style={thStyle}>Path</th>
                  <th scope="col" style={thStyle}>Filesystem</th>
                  <th scope="col" style={thStyle}>Rating</th>
                  <th scope="col" style={thStyle}>Why</th>
                </tr>
              </thead>
              <tbody>
                {report.mounts.map(mount => (
                  <tr key={mount.path}>
                    <td style={{ ...tdStyle, fontFamily: 'monospace', overflowWrap: 'anywhere' }}>{mount.path}</td>
                    <td style={tdStyle}>{mount.fs_type ?? 'unknown'}</td>
                    <td style={tdStyle}><Pill tone={TRUST_TEXT[mount.trust].tone}>{TRUST_TEXT[mount.trust].label}</Pill></td>
                    <td style={tdStyle}>{mount.reasons.join(' ')}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        <div style={{ ...subtleStyle, marginTop: 6 }}>
          The rating covers the filesystem layer only. It cannot see a disk&rsquo;s own volatile write cache or a RAID
          controller without battery backup.
        </div>
      </div>
    </section>
  )
}

/* ----------------------------- form controls ----------------------------- */

function Group({ title, children }: { title: string; children: React.ReactNode }) {
  const id = useId()
  return (
    <fieldset aria-labelledby={id} style={groupStyle}>
      <legend id={id} style={cardTitleStyle}>{title}</legend>
      <div style={{ display: 'grid', gap: 12 }}>{children}</div>
    </fieldset>
  )
}

function Help({ id, help, cost }: { id: string; help: string; cost?: string }) {
  return (
    <div id={id} style={helpStyle}>
      {help}
      {cost && <div style={{ marginTop: 3 }}>{cost}</div>}
    </div>
  )
}

function Toggle({ label, checked, onChange, help, cost, disabled }: {
  label: string
  checked: boolean
  onChange: (value: boolean) => void
  help: string
  cost?: string
  disabled?: boolean
}) {
  const id = useId()
  const helpId = `${id}-help`
  return (
    <div>
      <label htmlFor={id} style={{ display: 'flex', gap: 8, alignItems: 'center', fontSize: 13, fontWeight: 700, color: 'var(--text)' }}>
        <input id={id} type="checkbox" checked={checked} disabled={disabled} aria-describedby={helpId}
          onChange={event => onChange(event.target.checked)} />
        {label}
      </label>
      <Help id={helpId} help={help} cost={cost} />
    </div>
  )
}

function Select<T extends string>({ label, value, onChange, options, help, cost, disabled }: {
  label: string
  value: T
  onChange: (value: T) => void
  options: [T, string][]
  help: string
  cost?: string
  disabled?: boolean
}) {
  const id = useId()
  const helpId = `${id}-help`
  return (
    <div>
      <label htmlFor={id} style={fieldLabelStyle}>{label}</label>
      <select id={id} value={value} disabled={disabled} aria-describedby={helpId}
        onChange={event => onChange(event.target.value as T)} style={inputStyle}>
        {options.map(([optionValue, text]) => <option key={optionValue} value={optionValue}>{text}</option>)}
      </select>
      <Help id={helpId} help={help} cost={cost} />
    </div>
  )
}

function NumberField({ label, value, min, max, onChange, help, disabled }: {
  label: string
  value: number
  min: number
  max: number
  onChange: (value: number) => void
  help: string
  disabled?: boolean
}) {
  const id = useId()
  const helpId = `${id}-help`
  return (
    <div>
      <label htmlFor={id} style={fieldLabelStyle}>{label}</label>
      <input id={id} type="number" inputMode="numeric" min={min} max={max} value={Number.isFinite(value) ? value : ''}
        disabled={disabled} aria-describedby={helpId} style={{ ...inputStyle, maxWidth: 140 }}
        onChange={event => onChange(Number(event.target.value))} />
      <Help id={helpId} help={help} />
    </div>
  )
}

function PathList({ label, value, onChange, help }: { label: string; value: string; onChange: (value: string) => void; help: string }) {
  const id = useId()
  const helpId = `${id}-help`
  return (
    <div>
      <label htmlFor={id} style={fieldLabelStyle}>{label}</label>
      <textarea id={id} rows={3} value={value} spellCheck={false} aria-describedby={helpId}
        onChange={event => onChange(event.target.value)}
        style={{ ...inputStyle, fontFamily: 'monospace', resize: 'vertical' }} />
      <Help id={helpId} help={help} />
    </div>
  )
}

const INHERIT = ''

/** A select whose first choice is "inherit the global value" (value ''). */
function OverrideSelect<T extends string>({ label, value, onChange, options }: {
  label: string
  value: T | null
  onChange: (value: T | null) => void
  options: [T, string][]
}) {
  const id = useId()
  return (
    <div>
      <label htmlFor={id} style={fieldLabelStyle}>{label}</label>
      <select
        id={id}
        value={value ?? INHERIT}
        style={inputStyle}
        onChange={event => onChange(event.target.value === INHERIT ? null : (event.target.value as T))}
      >
        <option value={INHERIT}>Use global setting</option>
        {options.map(([optionValue, text]) => <option key={optionValue} value={optionValue}>{text}</option>)}
      </select>
    </div>
  )
}

function PolicyEditor({ index, policy, onChange, onRemove }: {
  index: number
  policy: CrashSafetyPathPolicy
  onChange: (next: CrashSafetyPathPolicy) => void
  onRemove: () => void
}) {
  const pathId = useId()
  const percentId = useId()
  const gate = policy.completion_gate === null ? null : policy.completion_gate ? 'on' : 'off'
  return (
    <fieldset style={{ ...groupStyle, background: 'color-mix(in srgb, var(--surface) 84%, var(--bg))' }} data-testid="path-policy">
      <legend style={cardTitleStyle}>Folder policy {index + 1}</legend>
      <div style={{ display: 'grid', gap: 10 }}>
        <div>
          <label htmlFor={pathId} style={fieldLabelStyle}>Folder</label>
          <input
            id={pathId}
            type="text"
            value={policy.path}
            placeholder="/mnt/nas"
            spellCheck={false}
            style={{ ...inputStyle, fontFamily: 'monospace' }}
            onChange={event => onChange({ ...policy, path: event.target.value })}
          />
          <div style={helpStyle}>Applies to torrents saved in this folder or anywhere below it.</div>
        </div>
        <div style={{ display: 'grid', gap: 10, gridTemplateColumns: 'repeat(auto-fit, minmax(190px, 1fr))' }}>
          <OverrideSelect<'on' | 'off'>
            label="Hold completion until synced"
            value={gate}
            onChange={value => onChange({ ...policy, completion_gate: value === null ? null : value === 'on' })}
            options={[['on', 'On'], ['off', 'Off']]}
          />
          <OverrideSelect<HostCrashRecovery>
            label="Recovery policy"
            value={policy.host_crash_recovery}
            onChange={value => onChange({ ...policy, host_crash_recovery: value })}
            options={Object.entries(RECOVERY_LABEL) as [HostCrashRecovery, string][]}
          />
          <OverrideSelect<StructuralAuditMode>
            label="Allocation audit"
            value={policy.structural_audit}
            onChange={value => onChange({ ...policy, structural_audit: value })}
            options={Object.entries(AUDIT_LABEL) as [StructuralAuditMode, string][]}
          />
          <OverrideSelect<CompletionVerifyMode>
            label="Read-back verification"
            value={policy.completion_verify}
            onChange={value => onChange({ ...policy, completion_verify: value })}
            options={Object.entries(VERIFY_LABEL) as [CompletionVerifyMode, string][]}
          />
          <div>
            <label htmlFor={percentId} style={fieldLabelStyle}>Sample size (percent)</label>
            <input
              id={percentId}
              type="number"
              inputMode="numeric"
              min={1}
              max={100}
              value={policy.completion_verify_sample_percent ?? ''}
              placeholder="Global"
              style={inputStyle}
              onChange={event =>
                onChange({
                  ...policy,
                  completion_verify_sample_percent: event.target.value === '' ? null : Number(event.target.value),
                })}
            />
          </div>
        </div>
        <div>
          <button type="button" style={secondaryButtonStyle} onClick={onRemove}
            aria-label={`Remove folder policy ${index + 1}${policy.path.trim() ? ` for ${policy.path.trim()}` : ''}`}>
            Remove
          </button>
        </div>
      </div>
    </fieldset>
  )
}

function PathReportView({ report }: { report: CrashSafetyPathReport }) {
  const trust = TRUST_TEXT[report.mount.trust]
  const recoveryRaised = report.host_crash_recovery_effective !== report.host_crash_recovery
  const verifyRaised = report.completion_verify_effective !== report.completion_verify
  const reason = report.recovery_escalated_by ? ESCALATION_TEXT[report.recovery_escalated_by] ?? report.recovery_escalated_by : null
  const verifyLabel = VERIFY_LABEL[report.completion_verify_effective]
  return (
    <dl style={{ ...cardStyle, display: 'grid', gridTemplateColumns: 'minmax(140px, max-content) 1fr', gap: '6px 14px', margin: '8px 0 0' }}
      data-testid="path-report">
      <Row label="Folder policy">
        {report.matched_policy
          ? <code>{report.matched_policy}</code>
          : 'None. The global settings apply.'}
      </Row>
      <Row label="Storage rating">
        <Pill tone={trust.tone}>{trust.label}</Pill>
        {' '}{report.mount.fs_type ?? 'unknown filesystem'}
        {report.mount.reasons.length > 0 && <div style={subtleStyle}>{report.mount.reasons.join(' ')}</div>}
      </Row>
      <Row label="Hold completion">{report.completion_gate ? 'On' : 'Off'}</Row>
      <Row label="Recovery after a power loss">
        {RECOVERY_LABEL[report.host_crash_recovery_effective]}
        {recoveryRaised && (
          <div style={subtleStyle}>
            Raised from {RECOVERY_LABEL[report.host_crash_recovery]}{reason ? ` because ${reason}` : ''}.
          </div>
        )}
      </Row>
      <Row label="Allocation audit">{AUDIT_LABEL[report.structural_audit]}</Row>
      <Row label="Read-back verification">
        {verifyLabel}{report.completion_verify_effective === 'sample' ? ` (${report.completion_verify_sample_percent}%)` : ''}
        {verifyRaised && (
          <div style={subtleStyle}>
            Raised from {VERIFY_LABEL[report.completion_verify]} because this location is rated Weak.
          </div>
        )}
      </Row>
    </dl>
  )
}

function Row({ label, children }: { label: string; children: React.ReactNode }) {
  return (
    <>
      <dt style={{ ...cardTitleStyle, marginBottom: 0, alignSelf: 'start', paddingTop: 2 }}>{label}</dt>
      <dd style={{ margin: 0, color: 'var(--muted)', fontSize: 12, overflowWrap: 'anywhere' }}>{children}</dd>
    </>
  )
}

function Stat({ label, value, hint }: { label: string; value: number; hint: string }) {
  return (
    <div title={hint}>
      <dt style={{ ...cardTitleStyle, marginBottom: 2 }}>{label}</dt>
      <dd style={{ margin: 0, fontSize: 18, fontWeight: 800, color: 'var(--text)' }}>{value.toLocaleString()}</dd>
    </div>
  )
}

function Pill({ tone, children }: { tone: 'ok' | 'info' | 'warn'; children: React.ReactNode }) {
  const color = tone === 'ok' ? 'var(--success)' : tone === 'warn' ? 'var(--warning)' : 'var(--accent)'
  return (
    <span style={{
      color,
      background: `color-mix(in srgb, ${color} 8%, transparent)`,
      border: `1px solid color-mix(in srgb, ${color} 45%, var(--border))`,
      borderRadius: 999,
      padding: '2px 8px',
      fontSize: 11,
      fontWeight: 800,
    }}>{children}</span>
  )
}

/* -------------------------------- styling -------------------------------- */

const sectionStyle: React.CSSProperties = { padding: '18px 20px', maxWidth: 860 }
const h2Style: React.CSSProperties = { margin: 0, fontSize: 14, fontWeight: 700, color: 'var(--text)' }
const subtleStyle: React.CSSProperties = { color: 'var(--faint)', fontSize: 12, marginTop: 3, lineHeight: 1.5 }
const paraStyle: React.CSSProperties = { color: 'var(--muted)', fontSize: 12, lineHeight: 1.55, margin: '8px 0 0' }
const cardStyle: React.CSSProperties = {
  background: 'color-mix(in srgb, var(--surface) 84%, var(--bg))',
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: 12,
  marginTop: 8,
}
const groupStyle: React.CSSProperties = {
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: '10px 12px 12px',
  margin: 0,
  minWidth: 0,
}
const cardTitleStyle: React.CSSProperties = {
  color: 'var(--faint)',
  fontSize: 10,
  fontWeight: 800,
  textTransform: 'uppercase',
  letterSpacing: 0,
  marginBottom: 6,
  padding: 0,
}
const statGridStyle: React.CSSProperties = {
  display: 'grid',
  gridTemplateColumns: 'repeat(auto-fit, minmax(130px, 1fr))',
  gap: 12,
  margin: '12px 0 0',
}
const helpStyle: React.CSSProperties = { color: 'var(--faint)', fontSize: 11, lineHeight: 1.5, marginTop: 4, maxWidth: 720 }
const fieldLabelStyle: React.CSSProperties = { display: 'block', fontSize: 13, fontWeight: 700, color: 'var(--text)', marginBottom: 4 }
const inputStyle: React.CSSProperties = {
  width: '100%',
  boxSizing: 'border-box',
  background: 'var(--bg)',
  border: '1px solid var(--border-strong)',
  borderRadius: 6,
  color: 'var(--text)',
  padding: '6px 9px',
  fontSize: 13,
}
const infoStyle: React.CSSProperties = { ...paraStyle, background: 'var(--surface)', border: '1px solid var(--border)', borderRadius: 7, padding: '9px 10px' }
const errorStyle: React.CSSProperties = {
  color: 'var(--danger)',
  background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
  border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
  borderRadius: 6,
  padding: '8px 9px',
  fontSize: 12,
  overflowWrap: 'anywhere',
}
const warnStyle: React.CSSProperties = {
  color: 'var(--warning)',
  background: 'color-mix(in srgb, var(--warning) 9%, var(--surface))',
  border: '1px solid color-mix(in srgb, var(--warning) 45%, var(--border))',
  borderRadius: 6,
  padding: '8px 9px',
  fontSize: 12,
  marginTop: 10,
}
const presetButtonStyle: React.CSSProperties = {
  background: 'var(--surface-2)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--muted)',
  padding: '4px 10px',
  fontSize: 11,
  fontWeight: 600,
  cursor: 'pointer',
}
const secondaryButtonStyle: React.CSSProperties = { ...presetButtonStyle, padding: '6px 14px', fontSize: 12 }
function primaryButtonStyle(enabled: boolean): React.CSSProperties {
  return {
    background: enabled ? 'var(--accent)' : 'var(--surface-2)',
    border: '1px solid ' + (enabled ? 'var(--accent)' : 'var(--border-strong)'),
    borderRadius: 6,
    color: enabled ? 'var(--accent-text)' : 'var(--faint)',
    padding: '6px 16px',
    fontSize: 12,
    cursor: enabled ? 'pointer' : 'default',
    opacity: enabled ? 1 : 0.6,
    fontWeight: 700,
  }
}
const tableStyle: React.CSSProperties = { width: '100%', borderCollapse: 'collapse', fontSize: 12 }
const thStyle: React.CSSProperties = { ...cardTitleStyle, textAlign: 'left', padding: '4px 8px', borderBottom: '1px solid var(--border)' }
const tdStyle: React.CSSProperties = { padding: '6px 8px', color: 'var(--muted)', verticalAlign: 'top', borderBottom: '1px solid var(--border)' }
const visuallyHidden: React.CSSProperties = {
  position: 'absolute', width: 1, height: 1, overflow: 'hidden', clip: 'rect(0 0 0 0)', whiteSpace: 'nowrap',
}
