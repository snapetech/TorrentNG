import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import { maskAnnounceUrl } from '../lib/maskUrl'
import type { WorkflowRule, WorkflowRun } from '../api/client'

const EMPTY: WorkflowRule = {
  id: '',
  name: '',
  enabled: true,
  event: 'completed',
  action: 'webhook',
  category: null,
  tracker: null,
  command: null,
  url: null,
  target_path: null,
}

export function WorkflowsPanel() {
  const qc = useQueryClient()
  const [draft, setDraft] = useState<WorkflowRule>(EMPTY)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [preview, setPreview] = useState<{ name: string; count: number } | null>(null)
  const { data: rules = [], isLoading: rulesLoading } = useQuery({
    queryKey: ['workflows'],
    queryFn: api.workflows.list,
    staleTime: 30_000,
  })
  const { data: runs = [] } = useQuery({
    queryKey: ['workflow-runs'],
    queryFn: api.workflows.runs,
    staleTime: 5_000,
    refetchInterval: 10_000,
  })

  async function save() {
    if (pending) return
    setPending('__save__')
    setError(null)
    try {
      await api.workflows.save({
        ...draft,
        name: draft.name.trim(),
        category: draft.category?.trim() || null,
        tracker: draft.tracker?.trim() || null,
        command: draft.command?.trim() || null,
        url: draft.url?.trim() || null,
        target_path: draft.target_path?.trim() || null,
      })
      setDraft(EMPTY)
      qc.invalidateQueries({ queryKey: ['workflows'] })
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  async function remove(id: string) {
    setPending(id)
    setError(null)
    try {
      await api.workflows.delete(id)
      qc.invalidateQueries({ queryKey: ['workflows'] })
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  async function run(rule: WorkflowRule, dryRun: boolean) {
    setPending(rule.id)
    setError(null)
    setPreview(null)
    try {
      const result = await api.workflows.run(rule.id, dryRun)
      if (dryRun) {
        setPreview({ name: rule.name, count: result.applied.length })
      } else if (result.errors.length > 0) {
        setError(`${result.errors.length} error(s) running ${rule.name}`)
      }
      qc.invalidateQueries({ queryKey: ['workflow-runs'] })
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
        <div>
          <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)' }}>Workflow Rules</div>
          <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 2 }}>
            {rules.length.toLocaleString()} rules · {runs.length.toLocaleString()} runs
          </div>
        </div>
        {pending && <Busy label={pending === '__save__' ? 'Saving' : 'Working'} />}
      </div>
      <PanelBox>
        <div style={{ fontSize: 12, fontWeight: 800, color: 'var(--text)', marginBottom: 10 }}>Workflow builder</div>
        <div style={scrollX}>
          <div style={{ display: 'grid', gridTemplateColumns: '150px 120px 120px 150px minmax(240px, 1fr) auto', gap: 8, minWidth: 860, maxWidth: 1080 }}>
            <Field label="Name"><Input value={draft.name} placeholder="notify complete" onChange={name => setDraft({ ...draft, name })} /></Field>
            <Field label="Event"><Select value={draft.event} onChange={event => setDraft({ ...draft, event: event as WorkflowRule['event'] })} options={['completed', 'added', 'category_changed']} /></Field>
            <Field label="Action"><Select value={draft.action} onChange={action => setDraft({ ...draft, action: action as WorkflowRule['action'] })} options={['webhook', 'script', 'set_category', 'set_location']} /></Field>
            <Field label="Filter"><Input value={draft.category ?? ''} placeholder="category" onChange={category => setDraft({ ...draft, category })} /></Field>
            <Field label="Target"><Input value={draft.url ?? draft.command ?? draft.target_path ?? ''} placeholder="URL, command, or path" onChange={value => setDraft({
              ...draft,
              url: draft.action === 'webhook' ? value : null,
              command: draft.action === 'script' ? value : null,
              target_path: draft.action === 'set_location' ? value : null,
              category: draft.action === 'set_category' ? value : draft.category,
            })} /></Field>
            <button onClick={save} disabled={!draft.name.trim() || Boolean(pending)} style={primaryButton(!draft.name.trim() || Boolean(pending))}>{pending === '__save__' ? 'Saving…' : 'Save'}</button>
          </div>
        </div>
      </PanelBox>
      {error && <Notice tone="error">{error}</Notice>}
      {preview && (
        <Notice tone="ok">
          {preview.name}: {preview.count.toLocaleString()} matching torrent{preview.count === 1 ? '' : 's'}
        </Notice>
      )}
      <div style={{ ...scrollX, display: 'grid', gap: 8, maxWidth: 1080 }}>
        {rulesLoading && <SkeletonRows count={3} />}
        {!rulesLoading && rules.length === 0 && (
          <EmptyState title="No workflow rules configured" detail="Create a workflow above, then preview it against the current torrent list before running." />
        )}
        {rules.map(rule => (
          <div key={rule.id} className="tng-automation-row" data-enabled={rule.enabled ? 'true' : 'false'} style={{
            display: 'grid', gridTemplateColumns: '150px 120px 120px 150px minmax(240px, 1fr) auto auto auto',
            minWidth: 980,
            gap: 8, alignItems: 'center', border: '1px solid var(--border)',
            borderRadius: 7, padding: '10px 12px', background: 'var(--surface)', fontSize: 12,
            }}>
            <strong style={{ color: 'var(--text)', display: 'inline-flex', alignItems: 'center', gap: 7 }}>
              <span aria-hidden="true" style={{
                width: 7, height: 7, borderRadius: 999,
                background: rule.enabled ? 'var(--success)' : 'var(--faint)',
              }} />
              {rule.name}
            </strong>
            <Pill tone="info">{rule.event}</Pill>
            <Pill tone={rule.enabled ? 'ok' : 'idle'}>{rule.action}</Pill>
            <span style={{ color: 'var(--faint)' }}>{rule.category || 'any category'}</span>
            <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {rule.url || rule.command || rule.target_path || (rule.tracker ? maskAnnounceUrl(rule.tracker) : null) || 'configured'}
            </span>
            <button onClick={() => run(rule, true)} disabled={Boolean(pending)} style={{
              background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
              color: 'var(--muted)', padding: '3px 8px', fontSize: 11,
              cursor: pending ? 'not-allowed' : 'pointer', opacity: pending ? 0.55 : 1,
            }}>Preview</button>
            <button onClick={() => run(rule, false)} disabled={Boolean(pending)} style={{
              background: 'var(--surface-2)', border: '1px solid var(--accent)', borderRadius: 4,
              color: 'var(--accent)', padding: '3px 8px', fontSize: 11,
              cursor: pending ? 'not-allowed' : 'pointer', opacity: pending ? 0.55 : 1,
            }}>Run</button>
            <button onClick={() => remove(rule.id)} disabled={Boolean(pending)} style={{
              background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
              color: 'var(--faint)', padding: '3px 8px', fontSize: 11,
              cursor: pending ? 'not-allowed' : 'pointer', opacity: pending ? 0.55 : 1,
            }}>Delete</button>
          </div>
        ))}
      </div>
      <div style={{ fontSize: 13, fontWeight: 700, marginTop: 20, marginBottom: 10, color: 'var(--text)' }}>
        Recent Runs
      </div>
      <div style={{ ...scrollX, display: 'grid', gap: 6, maxWidth: 1080 }}>
        {runs.slice(0, 8).map(run => <WorkflowRunRow key={run.id} run={run} />)}
        {runs.length === 0 && (
          <EmptyState title="No workflow runs recorded" detail="Preview or run a workflow to see recent activity here." />
        )}
      </div>
    </section>
  )
}

function Busy({ label }: { label: string }) {
  return <span style={{
    color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
    borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700,
  }}>{label}</span>
}

function Notice({ tone, children }: { tone: 'ok' | 'error'; children: React.ReactNode }) {
  return (
    <div style={{
      color: tone === 'error' ? 'var(--danger)' : 'var(--success)',
      background: tone === 'error' ? 'color-mix(in srgb, var(--danger) 9%, var(--surface))' : 'color-mix(in srgb, var(--success) 8%, var(--surface))',
      border: '1px solid ' + (tone === 'error' ? 'color-mix(in srgb, var(--danger) 45%, var(--border))' : 'color-mix(in srgb, var(--success) 40%, var(--border))'),
      borderRadius: 6, padding: '8px 9px', fontSize: 12, marginBottom: 10,
      overflowWrap: 'anywhere',
    }}>{children}</div>
  )
}

const scrollX: React.CSSProperties = {
  overflowX: 'auto',
  paddingBottom: 2,
}

function WorkflowRunRow({ run }: { run: WorkflowRun }) {
  const status = run.errors.length > 0 ? `${run.errors.length} error(s)` : run.dry_run ? 'previewed' : 'completed'
  const tone = run.errors.length > 0 ? 'error' : run.dry_run ? 'info' : 'ok'
  return (
    <div className="tng-automation-row" data-enabled={run.errors.length === 0 ? 'true' : 'false'} style={{
      display: 'grid', gridTemplateColumns: '150px 120px 90px 90px 90px 1fr',
      minWidth: 760,
      gap: 8, alignItems: 'center', border: '1px solid var(--border)',
      borderRadius: 6, padding: '8px 12px', background: 'var(--surface)', fontSize: 12,
    }}>
      <strong style={{ color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
        {run.rule_name}
      </strong>
      <span style={{ color: 'var(--muted)' }}>{run.action}</span>
      <span style={{ color: 'var(--faint)' }}>{run.matched.length.toLocaleString()} matched</span>
      <span style={{ color: 'var(--faint)' }}>{run.applied.length.toLocaleString()} applied</span>
      <Pill tone={tone}>{status}</Pill>
      <span style={{ color: 'var(--faint)', textAlign: 'right' }}>
        {new Date(run.started_at * 1000).toLocaleString()}
      </span>
    </div>
  )
}

function PanelBox({ children }: { children: React.ReactNode }) {
  return <div style={{
    maxWidth: 1120,
    background: 'color-mix(in srgb, var(--surface) 84%, var(--bg))',
    border: '1px solid var(--border)',
    borderRadius: 8,
    padding: 12,
    marginBottom: 12,
    boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.03)',
  }}>{children}</div>
}

function Field({ label, children }: { label: string; children: React.ReactNode }) {
  return <label style={{ display: 'grid', gap: 4, minWidth: 0 }}>
    <span style={{ color: 'var(--faint)', fontSize: 10, fontWeight: 800, textTransform: 'uppercase', letterSpacing: 0 }}>{label}</span>
    {children}
  </label>
}

function Pill({ tone, children }: { tone: 'ok' | 'info' | 'idle' | 'error'; children: React.ReactNode }) {
  const color = tone === 'ok' ? 'var(--success)' : tone === 'error' ? 'var(--danger)' : tone === 'info' ? 'var(--accent)' : 'var(--faint)'
  return <span style={{
    justifySelf: 'start',
    color,
    border: `1px solid color-mix(in srgb, ${color} 45%, var(--border))`,
    background: `color-mix(in srgb, ${color} 8%, transparent)`,
    borderRadius: 999,
    padding: '2px 8px',
    fontSize: 11,
    fontWeight: 800,
    whiteSpace: 'nowrap',
  }}>{children}</span>
}

function EmptyState({ title, detail }: { title: string; detail: string }) {
  return <div style={{
    border: '1px dashed var(--border-strong)', borderRadius: 8, padding: '16px 14px',
    background: 'color-mix(in srgb, var(--surface) 72%, transparent)', color: 'var(--muted)', fontSize: 12,
  }}>
    <strong style={{ display: 'block', color: 'var(--text)', marginBottom: 4 }}>{title}</strong>
    {detail}
  </div>
}

function SkeletonRows({ count }: { count: number }) {
  return Array.from({ length: count }, (_, index) => (
    <div key={index} style={{ border: '1px solid var(--border)', borderRadius: 7, padding: '12px', background: 'var(--surface)' }}>
      <span className="tng-skeleton" style={{ width: '30%', height: 12, marginBottom: 10 }} />
      <span className="tng-skeleton" style={{ width: '80%', height: 10 }} />
    </div>
  ))
}

function primaryButton(disabled = false): React.CSSProperties {
  return {
    alignSelf: 'end',
    background: 'var(--accent-soft)',
    border: '1px solid var(--accent)',
    borderRadius: 5,
    color: 'var(--accent-text)',
    padding: '5px 12px',
    fontSize: 12,
    fontWeight: 700,
    cursor: disabled ? 'not-allowed' : 'pointer',
    opacity: disabled ? 0.5 : 1,
  }
}

function Input({ value, placeholder, onChange }: { value: string; placeholder: string; onChange: (value: string) => void }) {
  return <input value={value} placeholder={placeholder} onChange={e => onChange(e.target.value)} style={{
    minWidth: 0, background: 'var(--bg)', border: '1px solid var(--border-strong)',
    borderRadius: 5, color: 'var(--text)', padding: '4px 8px', fontSize: 12,
  }} />
}

function Select({ value, options, onChange }: { value: string; options: string[]; onChange: (value: string) => void }) {
  return <select value={value} onChange={e => onChange(e.target.value)} style={{
    minWidth: 0, background: 'var(--bg)', border: '1px solid var(--border-strong)',
    borderRadius: 5, color: 'var(--text)', padding: '4px 8px', fontSize: 12,
  }}>
    {options.map(option => <option key={option} value={option}>{option}</option>)}
  </select>
}
