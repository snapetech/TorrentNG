import { useMemo, useState } from 'react'
import { useMutation, useQuery } from '@tanstack/react-query'
import { api, type Job, type StoragePlanRequest, type StoragePlanResponse } from '../api/client'

function fmtBytes(bytes: number): string {
  if (bytes >= 1e12) return (bytes / 1e12).toFixed(2) + ' TB'
  if (bytes >= 1e9) return (bytes / 1e9).toFixed(2) + ' GB'
  if (bytes >= 1e6) return (bytes / 1e6).toFixed(1) + ' MB'
  if (bytes >= 1e3) return (bytes / 1e3).toFixed(0) + ' KB'
  return bytes + ' B'
}

function parseLines(value: string): string[] {
  return value
    .split(/[\n,]/)
    .map(part => part.trim())
    .filter(Boolean)
}

function parseStepIndexes(value: string): number[] {
  return Array.from(new Set(parseLines(value)
    .map(part => Number(part))
    .filter(index => Number.isInteger(index) && index >= 0)))
    .sort((left, right) => left - right)
}

type StoragePlanTemplate = {
  id: string
  label: string
  operation: StoragePlanRequest['operation']
  sourceSuffix?: string
  destinationSuffix?: string
  targetSuffix?: string
  hardlinkOrCopy?: boolean
  deleteApproved?: boolean
  completedSteps?: string
}

const storagePlanTemplates: StoragePlanTemplate[] = [
  { id: 'move-library', label: 'Move', operation: 'move', sourceSuffix: 'library/source', destinationSuffix: 'library/destination' },
  { id: 'import-copy', label: 'Import copy', operation: 'import', sourceSuffix: 'staging/source', destinationSuffix: 'library/imported', hardlinkOrCopy: false },
  { id: 'import-link', label: 'Import link', operation: 'import', sourceSuffix: 'staging/source', destinationSuffix: 'library/linked', hardlinkOrCopy: true },
  { id: 'delete-approved', label: 'Delete', operation: 'delete', targetSuffix: 'library/orphaned', deleteApproved: true },
  { id: 'resume-plan', label: 'Resume', operation: 'move', sourceSuffix: 'library/source', destinationSuffix: 'library/destination', completedSteps: '0' },
]

function joinRootPath(root: string, suffix: string): string {
  const clean = root.replace(/\/+$/, '')
  return clean ? `${clean}/${suffix}` : suffix
}

export function StoragePanel() {
  const [operation, setOperation] = useState<StoragePlanRequest['operation']>('move')
  const [source, setSource] = useState('')
  const [destination, setDestination] = useState('')
  const [target, setTarget] = useState('')
  const [bytes, setBytes] = useState('')
  const [rootPath, setRootPath] = useState('')
  const [hardlinkOrCopy, setHardlinkOrCopy] = useState(false)
  const [deleteApproved, setDeleteApproved] = useState(false)
  const [affectedTorrents, setAffectedTorrents] = useState('')
  const [completedSteps, setCompletedSteps] = useState('')
  const [preview, setPreview] = useState<StoragePlanResponse | null>(null)
  const { data, isLoading, isFetching, error, refetch } = useQuery({
    queryKey: ['storage'],
    queryFn: api.storage,
    staleTime: 5_000,
    refetchInterval: 10_000,
  })
  const { data: jobsData } = useQuery({
    queryKey: ['jobs', 'storage-plan'],
    queryFn: api.jobs,
    staleTime: 2_000,
    refetchInterval: 3_000,
  })
  const roots = data?.roots.filter(root => root.ok && !root.readonly) ?? []
  const storageJobs = jobsData?.jobs.filter(job => job.kind === 'storage_plan') ?? []
  const selectedRoot = rootPath || roots[0]?.path || ''
  const selectedRootInfo = roots.find(root => root.path === selectedRoot)
  const request = useMemo<StoragePlanRequest>(() => {
    const parsedBytes = bytes.trim() ? Number(bytes.trim()) : undefined
    const affected = parseLines(affectedTorrents)
    const completed = parseStepIndexes(completedSteps)
    return {
      operation,
      source: operation === 'delete' ? undefined : source.trim() || undefined,
      destination: operation === 'delete' ? undefined : destination.trim() || undefined,
      target: operation === 'delete' ? target.trim() || undefined : undefined,
      bytes: parsedBytes !== undefined && Number.isFinite(parsedBytes) && parsedBytes >= 0 ? parsedBytes : undefined,
      available_bytes: selectedRootInfo?.available_bytes,
      hardlink_or_copy: operation === 'import' ? hardlinkOrCopy : undefined,
      dry_run: true,
      dry_run_approved: operation === 'delete' ? deleteApproved : undefined,
      roots: selectedRoot ? [selectedRoot] : undefined,
      affected_torrents: affected.length ? affected : undefined,
      completed_steps: completed.length ? completed : undefined,
    }
  }, [operation, source, destination, target, bytes, selectedRoot, selectedRootInfo?.available_bytes, hardlinkOrCopy, deleteApproved, affectedTorrents, completedSteps])
  const previewPlan = useMutation({
    mutationFn: () => api.storagePlan.preview(request),
    onSuccess: setPreview,
  })
  const executePlan = useMutation({
    mutationFn: () => api.storagePlan.execute({
      ...request,
      dry_run: false,
      // Preview roots are advisory UI state. The server owns execution roots.
      roots: undefined,
      dry_run_approved: operation === 'delete' ? deleteApproved : undefined,
    }),
    onSuccess: setPreview,
  })
  const canExecute = Boolean(preview?.plan.can_apply && selectedRoot && (operation !== 'delete' || deleteApproved))
  const completedIndexes = useMemo(() => parseStepIndexes(completedSteps), [completedSteps])
  const invalidCompletedIndexes = useMemo(() => {
    if (!preview) return []
    return completedIndexes.filter(index => index >= preview.plan.steps.length)
  }, [completedIndexes, preview])
  const canExecutePlan = canExecute && invalidCompletedIndexes.length === 0
  const applyTemplate = (template: StoragePlanTemplate) => {
    setOperation(template.operation)
    setHardlinkOrCopy(Boolean(template.hardlinkOrCopy))
    setDeleteApproved(Boolean(template.deleteApproved))
    setCompletedSteps(template.completedSteps ?? '')
    setBytes('')
    setAffectedTorrents('')
    if (template.operation === 'delete') {
      setSource('')
      setDestination('')
      setTarget(template.targetSuffix && selectedRoot ? joinRootPath(selectedRoot, template.targetSuffix) : '')
    } else {
      setSource(template.sourceSuffix && selectedRoot ? joinRootPath(selectedRoot, template.sourceSuffix) : '')
      setDestination(template.destinationSuffix && selectedRoot ? joinRootPath(selectedRoot, template.destinationSuffix) : '')
      setTarget('')
    }
    setPreview(null)
  }

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 12 }}>
        <div style={{ fontSize: 14, fontWeight: 600, color: 'var(--text)', flex: 1 }}>
          Storage
        </div>
        <button
          onClick={() => refetch()}
          disabled={isFetching}
          style={{
            background: 'none', border: '1px solid var(--border-strong)', borderRadius: 5,
            color: 'var(--muted)', padding: '4px 9px', fontSize: 12,
            cursor: isFetching ? 'not-allowed' : 'pointer', opacity: isFetching ? 0.55 : 1,
          }}
        >
          {isFetching ? 'Refreshing…' : 'Refresh'}
        </button>
      </div>

      {isLoading && <SkeletonRows rows={2} />}
      {error && <Notice>Storage stats unavailable</Notice>}

      <div style={{ display: 'grid', gap: 10, maxWidth: 840 }}>
        {data && data.roots.length === 0 && (
          <EmptyState>No storage roots reported.</EmptyState>
        )}
        {data?.roots.map(root => (
          <StorageRootCard
            key={root.path}
            root={root}
          />
        ))}
      </div>

      <div style={{ marginTop: 14, maxWidth: 960 }}>
        {storageJobs.length > 0 && (
          <StorageJobs jobs={storageJobs} />
        )}

        <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 10, flexWrap: 'wrap' }}>
          <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)', flex: 1, minWidth: 200 }}>
            Storage Plan
          </div>
          <select value={operation} onChange={event => {
            setOperation(event.target.value as StoragePlanRequest['operation'])
            setPreview(null)
          }} style={fieldStyle} aria-label="Storage operation">
            <option value="move">Move</option>
            <option value="import">Import</option>
            <option value="delete">Delete</option>
          </select>
          <select value={selectedRoot} onChange={event => {
            setRootPath(event.target.value)
            setPreview(null)
          }} style={fieldStyle} aria-label="Storage root">
            {!selectedRoot && <option value="">Select root</option>}
            {roots.map(root => <option key={root.path} value={root.path}>{root.path}</option>)}
          </select>
        </div>

        <div style={{ display: 'flex', alignItems: 'center', gap: 7, flexWrap: 'wrap', marginBottom: 10 }}>
          <span style={{ color: 'var(--faint)', fontSize: 11, fontWeight: 800, textTransform: 'uppercase' }}>Templates</span>
          {storagePlanTemplates.map(template => (
            <button
              key={template.id}
              type="button"
              onClick={() => applyTemplate(template)}
              style={{
                background: template.operation === operation ? 'color-mix(in srgb, var(--accent) 14%, var(--surface))' : 'var(--surface)',
                border: '1px solid ' + (template.operation === operation ? 'var(--accent)' : 'var(--border)'),
                borderRadius: 5,
                color: template.operation === operation ? 'var(--accent-text)' : 'var(--muted)',
                padding: '4px 8px',
                fontSize: 12,
                fontWeight: 800,
                cursor: 'pointer',
              }}
            >
              {template.label}
            </button>
          ))}
        </div>

        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))', gap: 9, marginBottom: 10 }}>
          {operation === 'delete' ? (
            <PathField label="Target" value={target} onChange={value => { setTarget(value); setPreview(null) }} />
          ) : (
            <>
              <PathField label="Source" value={source} onChange={value => { setSource(value); setPreview(null) }} />
              <PathField label="Destination" value={destination} onChange={value => { setDestination(value); setPreview(null) }} />
            </>
          )}
          <label style={labelStyle}>
            <span>Expected bytes</span>
            <input value={bytes} onChange={event => { setBytes(event.target.value); setPreview(null) }} inputMode="numeric" placeholder="Optional" style={fieldStyle} />
          </label>
        </div>

        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(260px, 1fr))', gap: 9, marginBottom: 10 }}>
          <label style={labelStyle}>
            <span>Affected torrents</span>
            <textarea
              value={affectedTorrents}
              onChange={event => { setAffectedTorrents(event.target.value); setPreview(null) }}
              rows={3}
              placeholder="Info hashes, one per line"
              style={{ ...fieldStyle, resize: 'vertical', minHeight: 76 }}
            />
          </label>
          <label style={labelStyle}>
            <span>Completed steps</span>
            <textarea
              value={completedSteps}
              onChange={event => { setCompletedSteps(event.target.value); setPreview(null) }}
              rows={3}
              placeholder="0, 1, 2"
              style={{ ...fieldStyle, resize: 'vertical', minHeight: 76 }}
            />
          </label>
        </div>

        <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap', alignItems: 'center', marginBottom: 10 }}>
          {operation === 'import' && (
            <label style={checkStyle}>
              <input type="checkbox" checked={hardlinkOrCopy} onChange={event => { setHardlinkOrCopy(event.target.checked); setPreview(null) }} />
              <span>Allow hardlink/copy import</span>
            </label>
          )}
          {operation === 'delete' && (
            <label style={checkStyle}>
              <input type="checkbox" checked={deleteApproved} onChange={event => { setDeleteApproved(event.target.checked); setPreview(null) }} />
              <span>Approve delete execution</span>
            </label>
          )}
          <button onClick={() => previewPlan.mutate()} disabled={previewPlan.isPending || !selectedRoot} style={actionButtonStyle(!previewPlan.isPending && Boolean(selectedRoot))}>
            {previewPlan.isPending ? 'Previewing...' : 'Preview plan'}
          </button>
          <button onClick={() => executePlan.mutate()} disabled={executePlan.isPending || !canExecutePlan} style={actionButtonStyle(!executePlan.isPending && canExecutePlan)}>
            {executePlan.isPending ? 'Starting...' : 'Execute plan'}
          </button>
        </div>

        {invalidCompletedIndexes.length > 0 && (
          <Notice>Completed steps outside this plan: {invalidCompletedIndexes.join(', ')}</Notice>
        )}
        {previewPlan.error && <Notice>Plan preview failed</Notice>}
        {executePlan.error && <Notice>Plan execution failed</Notice>}
        {preview && <StoragePlanResult response={preview} completedIndexes={completedIndexes} />}
      </div>
    </section>
  )
}

function StorageJobs({ jobs }: { jobs: Job[] }) {
  return (
    <div style={{ display: 'grid', gap: 7, marginBottom: 12 }}>
      {jobs.map(job => (
        <div
          key={job.job_id}
          style={{
            border: '1px solid var(--border)',
            borderRadius: 7,
            background: 'var(--surface)',
            padding: 10,
            display: 'grid',
            gap: 7,
          }}
        >
          <div style={{ display: 'flex', gap: 8, alignItems: 'center', minWidth: 0 }}>
            <strong style={{ color: 'var(--text)', fontSize: 12, textTransform: 'capitalize' }}>
              {job.state}
            </strong>
            <code style={{ color: 'var(--faint)', fontSize: 11, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {job.job_id}
            </code>
            <span style={{ marginLeft: 'auto', color: 'var(--muted)', fontSize: 12, fontWeight: 800 }}>
              {Math.round(job.progress * 100)}%
            </span>
          </div>
          <div style={{ height: 7, borderRadius: 99, background: 'var(--surface-2)', overflow: 'hidden' }}>
            <div style={{ width: `${Math.max(0, Math.min(100, job.progress * 100))}%`, height: '100%', background: 'var(--accent)' }} />
          </div>
          <div style={{ display: 'flex', flexWrap: 'wrap', gap: 7 }}>
            <SummaryPill label="steps" value={`${job.done}/${job.total}`} />
            <SummaryPill label="checkpoint" value={String(job.checkpoint)} />
            <SummaryPill label="bytes" value={fmtBytes(Math.max(0, job.byte_offset ?? job.verified_bytes))} />
            {job.affected_torrents.length > 0 && (
              <SummaryPill label="torrents" value={String(job.affected_torrents.length)} />
            )}
          </div>
        </div>
      ))}
    </div>
  )
}

function PathField({ label, value, onChange }: { label: string; value: string; onChange: (value: string) => void }) {
  return (
    <label style={labelStyle}>
      <span>{label}</span>
      <input value={value} onChange={event => onChange(event.target.value)} placeholder="/path/on/storage/root" style={fieldStyle} />
    </label>
  )
}

function StoragePlanResult({ response, completedIndexes }: { response: StoragePlanResponse; completedIndexes: number[] }) {
  const tone = response.plan.can_apply ? 'var(--success)' : 'var(--warning)'
  const totalBytes = response.plan.steps.reduce((sum, step) => sum + step.bytes, 0)
  const rollbackBytes = response.plan.rollback_steps.reduce((sum, step) => sum + step.bytes, 0)
  const completedSet = new Set(completedIndexes)
  const completed = completedIndexes.filter(index => index < response.plan.steps.length).length
  const remaining = Math.max(0, response.plan.steps.length - completed)
  return (
    <div style={{ border: `1px solid color-mix(in srgb, ${tone} 38%, var(--border))`, borderRadius: 7, background: 'var(--surface)', padding: 12 }}>
      <div style={{ display: 'flex', gap: 10, alignItems: 'center', marginBottom: 8 }}>
        <strong style={{ color: 'var(--text)', fontSize: 13, textTransform: 'capitalize' }}>{response.operation}</strong>
        <span style={{ color: tone, fontSize: 12, fontWeight: 800 }}>{response.plan.can_apply ? 'Can apply' : 'Needs attention'}</span>
        {response.job_id && <code style={{ color: 'var(--faint)', fontSize: 11 }}>{response.job_id}</code>}
      </div>
      <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8, marginBottom: 9 }}>
        <SummaryPill label="steps" value={String(response.plan.steps.length)} />
        <SummaryPill label="done" value={String(completed)} />
        <SummaryPill label="remaining" value={String(remaining)} />
        <SummaryPill label="bytes" value={fmtBytes(totalBytes)} />
        <SummaryPill label="rollback" value={`${response.plan.rollback_steps.length} / ${fmtBytes(rollbackBytes)}`} />
      </div>
      {response.plan.issues.length > 0 && (
        <ul style={{ margin: '0 0 9px 16px', padding: 0, color: 'var(--warning)', fontSize: 12 }}>
          {response.plan.issues.map(issue => <li key={issue}>{issue}</li>)}
        </ul>
      )}
      <div style={{ display: 'grid', gap: 6 }}>
        {response.plan.steps.map((step, index) => {
          const done = completedSet.has(index)
          return (
          <div key={`${step.action}-${index}`} style={{
            display: 'grid',
            gridTemplateColumns: '132px 1fr auto',
            gap: 8,
            alignItems: 'center',
            border: `1px solid ${done ? 'color-mix(in srgb, var(--success) 42%, var(--border))' : 'var(--border)'}`,
            borderRadius: 6,
            padding: '7px 9px',
            color: 'var(--muted)',
            fontSize: 12,
            background: done ? 'color-mix(in srgb, var(--success) 7%, transparent)' : undefined,
          }}>
            <strong style={{ color: 'var(--text)' }}>{done ? 'done' : step.action}</strong>
            <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={[step.source, step.destination].filter(Boolean).join(' -> ')}>
              {[step.source, step.destination].filter(Boolean).join(' -> ') || 'storage operation'}
            </span>
            <span>{fmtBytes(step.bytes)}</span>
          </div>
          )
        })}
      </div>
      {response.plan.rollback_steps.length > 0 && (
        <details style={{ marginTop: 9, color: 'var(--muted)', fontSize: 12 }}>
          <summary style={{ cursor: 'pointer', color: 'var(--accent-text)', fontWeight: 800 }}>Rollback steps</summary>
          <div style={{ display: 'grid', gap: 6, marginTop: 7 }}>
            {response.plan.rollback_steps.map((step, index) => (
              <div key={`${step.action}-rollback-${index}`} style={{
                display: 'grid',
                gridTemplateColumns: '132px 1fr auto',
                gap: 8,
                alignItems: 'center',
                border: '1px solid var(--border)',
                borderRadius: 6,
                padding: '7px 9px',
              }}>
                <strong style={{ color: 'var(--text)' }}>{step.action}</strong>
                <span style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }} title={[step.source, step.destination].filter(Boolean).join(' -> ')}>
                  {[step.source, step.destination].filter(Boolean).join(' -> ') || 'storage operation'}
                </span>
                <span>{fmtBytes(step.bytes)}</span>
              </div>
            ))}
          </div>
        </details>
      )}
    </div>
  )
}

function SummaryPill({ label, value }: { label: string; value: string }) {
  return (
    <span style={{
      display: 'inline-flex',
      gap: 5,
      alignItems: 'center',
      border: '1px solid var(--border)',
      borderRadius: 999,
      padding: '2px 8px',
      color: 'var(--muted)',
      fontSize: 11,
    }}>
      <span style={{ color: 'var(--faint)' }}>{label}</span>
      <strong style={{ color: 'var(--text)' }}>{value}</strong>
    </span>
  )
}

function StorageRootCard({ root }: { root: NonNullable<Awaited<ReturnType<typeof api.storage>>['roots']>[number] }) {
  const tone = !root.ok
    ? 'var(--danger)'
    : root.used_percent >= 90
      ? 'var(--danger)'
      : root.used_percent >= 75
        ? 'var(--warning)'
        : 'var(--success)'
  return (
    <div
      className="tng-card tng-storage-root"
      data-tone={!root.ok ? 'error' : root.used_percent >= 90 ? 'error' : root.used_percent >= 75 ? 'warn' : 'ok'}
      style={{
        border: '1px solid var(--border)',
        borderRadius: 7,
        padding: 12,
        background: 'var(--surface)',
      }}
    >
      <div style={{ display: 'flex', alignItems: 'center', gap: 10, marginBottom: 8 }}>
        <div style={{
          flex: 1, minWidth: 0, color: root.ok ? 'var(--text)' : 'var(--danger)',
          fontSize: 13, fontFamily: 'monospace', overflow: 'hidden',
          textOverflow: 'ellipsis', whiteSpace: 'nowrap',
        }} title={root.path}>
          {root.path}
        </div>
        {root.readonly && (
          <span style={{
            color: 'var(--warning)', border: '1px solid color-mix(in srgb, var(--warning) 45%, var(--border))',
            background: 'color-mix(in srgb, var(--warning) 9%, transparent)', borderRadius: 999,
            padding: '1px 7px', fontSize: 11, fontWeight: 700,
          }}>read-only</span>
        )}
        <span style={{ color: tone, fontSize: 12, fontWeight: 700 }}>
          {root.ok ? `${root.used_percent.toFixed(1)}% used` : 'unavailable'}
        </span>
      </div>

      {root.ok ? (
        <>
          <div className="tng-storage-meter" style={{ height: 8, background: 'var(--surface-2)', borderRadius: 99, overflow: 'hidden', marginBottom: 9 }}>
            <div style={{ width: `${Math.min(100, root.used_percent)}%`, height: '100%', background: tone }} />
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(120px, 1fr))', gap: 8, fontSize: 12 }}>
            <StorageMetric label="Used" value={fmtBytes(root.used_bytes)} />
            <StorageMetric label="Free" value={fmtBytes(root.available_bytes)} />
            <StorageMetric label="Total" value={fmtBytes(root.total_bytes)} />
          </div>
        </>
      ) : (
        <div style={{ color: 'var(--danger)', fontSize: 12 }}>{root.error}</div>
      )}
    </div>
  )
}

function SkeletonRows({ rows }: { rows: number }) {
  return (
    <div style={{ display: 'grid', gap: 10, maxWidth: 840 }}>
      {Array.from({ length: rows }).map((_, index) => (
        <div key={index} style={{
          border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)', padding: 12,
          display: 'grid', gap: 9,
        }}>
          <span className="tng-skeleton" style={{ width: '55%', height: 12 }} />
          <span className="tng-skeleton" style={{ width: '100%', height: 8 }} />
          <span className="tng-skeleton" style={{ width: '72%', height: 24 }} />
        </div>
      ))}
    </div>
  )
}

function Notice({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--danger)', background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
      border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
      borderRadius: 6, padding: '8px 9px', fontSize: 12, marginBottom: 10,
    }}>{children}</div>
  )
}

function EmptyState({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--faint)', fontSize: 12, border: '1px dashed var(--border-strong)',
      borderRadius: 7, background: 'color-mix(in srgb, var(--surface) 72%, transparent)', padding: 14,
    }}>{children}</div>
  )
}

function StorageMetric({ label, value }: { label: string; value: string }) {
  return (
    <span className="tng-metric-tile" style={{
      display: 'grid', gap: 2, border: '1px solid var(--border)', borderRadius: 6,
      background: 'var(--bg)', padding: '6px 8px', minWidth: 0,
    }}>
      <span style={{ color: 'var(--faint)', fontSize: 10, textTransform: 'uppercase', fontWeight: 700 }}>{label}</span>
      <span style={{ color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{value}</span>
    </span>
  )
}

const fieldStyle: React.CSSProperties = {
  background: 'var(--surface)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '6px 8px',
  fontSize: 12,
  minWidth: 0,
}

const labelStyle: React.CSSProperties = {
  display: 'grid',
  gap: 4,
  color: 'var(--faint)',
  fontSize: 11,
  fontWeight: 700,
  minWidth: 0,
}

const checkStyle: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  gap: 6,
  color: 'var(--muted)',
  fontSize: 12,
}

function actionButtonStyle(enabled: boolean): React.CSSProperties {
  return {
    background: enabled ? 'var(--accent)' : 'var(--surface-2)',
    border: '1px solid ' + (enabled ? 'var(--accent)' : 'var(--border)'),
    borderRadius: 5,
    color: enabled ? 'var(--accent-text)' : 'var(--faint)',
    padding: '6px 10px',
    fontSize: 12,
    fontWeight: 800,
    cursor: enabled ? 'pointer' : 'not-allowed',
  }
}
