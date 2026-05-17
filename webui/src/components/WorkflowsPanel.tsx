import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
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
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 12, color: 'var(--text)' }}>
        Workflow Rules
      </div>
      <div style={scrollX}>
        <div style={{ display: 'grid', gridTemplateColumns: '150px 120px 120px 150px minmax(240px, 1fr) auto', gap: 8, minWidth: 860, maxWidth: 1080, marginBottom: 12 }}>
          <Input value={draft.name} placeholder="Name" onChange={name => setDraft({ ...draft, name })} />
          <Select value={draft.event} onChange={event => setDraft({ ...draft, event: event as WorkflowRule['event'] })} options={['completed', 'added', 'category_changed']} />
          <Select value={draft.action} onChange={action => setDraft({ ...draft, action: action as WorkflowRule['action'] })} options={['webhook', 'script', 'set_category', 'set_location']} />
          <Input value={draft.category ?? ''} placeholder="Category filter" onChange={category => setDraft({ ...draft, category })} />
          <Input value={draft.url ?? draft.command ?? draft.target_path ?? ''} placeholder="URL, command, or path" onChange={value => setDraft({
            ...draft,
            url: draft.action === 'webhook' ? value : null,
            command: draft.action === 'script' ? value : null,
            target_path: draft.action === 'set_location' ? value : null,
            category: draft.action === 'set_category' ? value : draft.category,
          })} />
          <button onClick={save} disabled={!draft.name.trim() || Boolean(pending)} style={{
            background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
            color: 'var(--accent-text)', padding: '4px 10px', fontSize: 12,
            cursor: draft.name.trim() && !pending ? 'pointer' : 'not-allowed', opacity: draft.name.trim() && !pending ? 1 : 0.5,
          }}>{pending === '__save__' ? 'Saving…' : 'Save'}</button>
        </div>
      </div>
      {error && <div style={{ color: '#ef4444', fontSize: 12, marginBottom: 10 }}>{error}</div>}
      {preview && (
        <div style={{ color: 'var(--muted)', fontSize: 12, marginBottom: 10 }}>
          {preview.name}: {preview.count.toLocaleString()} matching torrent{preview.count === 1 ? '' : 's'}
        </div>
      )}
      <div style={{ ...scrollX, display: 'grid', gap: 8, maxWidth: 1080 }}>
        {rulesLoading && <div style={{ color: 'var(--faint)', fontSize: 12 }}>Loading workflow rules…</div>}
        {!rulesLoading && rules.length === 0 && (
          <div style={{ color: 'var(--faint)', fontSize: 12, padding: '8px 0' }}>No workflow rules configured.</div>
        )}
        {rules.map(rule => (
          <div key={rule.id} style={{
            display: 'grid', gridTemplateColumns: '150px 120px 120px 150px minmax(240px, 1fr) auto auto auto',
            minWidth: 980,
            gap: 8, alignItems: 'center', border: '1px solid var(--border)',
            borderRadius: 6, padding: '9px 12px', background: 'var(--surface)', fontSize: 12,
          }}>
            <strong style={{ color: 'var(--text)' }}>{rule.name}</strong>
            <span style={{ color: 'var(--muted)' }}>{rule.event}</span>
            <span style={{ color: 'var(--muted)' }}>{rule.action}</span>
            <span style={{ color: 'var(--faint)' }}>{rule.category || 'any category'}</span>
            <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {rule.url || rule.command || rule.target_path || rule.tracker || 'configured'}
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
      <div style={{ fontSize: 13, fontWeight: 600, marginTop: 20, marginBottom: 10, color: 'var(--text)' }}>
        Recent Runs
      </div>
      <div style={{ ...scrollX, display: 'grid', gap: 6, maxWidth: 1080 }}>
        {runs.slice(0, 8).map(run => <WorkflowRunRow key={run.id} run={run} />)}
        {runs.length === 0 && (
          <div style={{ color: 'var(--faint)', fontSize: 12, padding: '8px 0' }}>
            No workflow runs recorded.
          </div>
        )}
      </div>
    </section>
  )
}

const scrollX: React.CSSProperties = {
  overflowX: 'auto',
  paddingBottom: 2,
}

function WorkflowRunRow({ run }: { run: WorkflowRun }) {
  const status = run.errors.length > 0 ? `${run.errors.length} error(s)` : run.dry_run ? 'previewed' : 'completed'
  return (
    <div style={{
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
      <span style={{ color: run.errors.length > 0 ? '#ef4444' : '#22c55e' }}>{status}</span>
      <span style={{ color: 'var(--faint)', textAlign: 'right' }}>
        {new Date(run.started_at * 1000).toLocaleString()}
      </span>
    </div>
  )
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
