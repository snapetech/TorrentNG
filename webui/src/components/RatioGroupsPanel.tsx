import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import type { RatioGroup } from '../api/client'

const EMPTY: RatioGroup = {
  name: '',
  ratio_limit: 1,
  seeding_time_limit: -1,
  category: null,
  tracker: null,
  enabled: true,
}

export function RatioGroupsPanel() {
  const qc = useQueryClient()
  const [draft, setDraft] = useState<RatioGroup>(EMPTY)
  const [error, setError] = useState<string | null>(null)
  const [pending, setPending] = useState<string | null>(null)
  const [preview, setPreview] = useState<{ name: string; count: number } | null>(null)

  const { data: groups = [], isLoading } = useQuery({
    queryKey: ['ratio-groups'],
    queryFn: api.ratioGroups.list,
    staleTime: 30_000,
  })

  async function save() {
    if (pending) return
    setPending('__save__')
    setError(null)
    try {
      await api.ratioGroups.save({
        ...draft,
        name: draft.name.trim(),
        category: draft.category?.trim() || null,
        tracker: draft.tracker?.trim() || null,
      })
      setDraft(EMPTY)
      qc.invalidateQueries({ queryKey: ['ratio-groups'] })
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  async function remove(name: string) {
    setPending(name)
    setError(null)
    try {
      await api.ratioGroups.delete(name)
      qc.invalidateQueries({ queryKey: ['ratio-groups'] })
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  async function apply(name: string, dryRun: boolean) {
    setPending(name)
    setError(null)
    setPreview(null)
    try {
      const result = await api.ratioGroups.apply(name, dryRun)
      if (dryRun) {
        setPreview({ name, count: result.applied.length })
      } else if (result.errors.length > 0) {
        setError(`${result.errors.length} error(s) applying ${name}`)
      }
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
          <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)' }}>Ratio Groups</div>
          <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 2 }}>{groups.length.toLocaleString()} configured</div>
        </div>
        {pending && <Busy label={pending === '__save__' ? 'Saving' : 'Working'} />}
      </div>

      <PanelBox>
        <div style={{ fontSize: 12, fontWeight: 800, color: 'var(--text)', marginBottom: 10 }}>Create or update group</div>
        <div style={scrollX}>
          <div style={{ display: 'grid', gridTemplateColumns: '160px 90px 110px 150px minmax(220px, 1fr) auto', gap: 8, minWidth: 820, maxWidth: 980 }}>
            <Field label="Name"><Input value={draft.name} placeholder="ebooks-strict" onChange={name => setDraft({ ...draft, name })} /></Field>
            <Field label="Ratio"><Input value={String(draft.ratio_limit)} placeholder="1.5" onChange={value => setDraft({ ...draft, ratio_limit: Number(value) })} /></Field>
            <Field label="Seed min"><Input value={String(draft.seeding_time_limit)} placeholder="-1" onChange={value => setDraft({ ...draft, seeding_time_limit: Number(value) })} /></Field>
            <Field label="Category"><Input value={draft.category ?? ''} placeholder="optional" onChange={category => setDraft({ ...draft, category })} /></Field>
            <Field label="Tracker"><Input value={draft.tracker ?? ''} placeholder="contains" onChange={tracker => setDraft({ ...draft, tracker })} /></Field>
            <button onClick={save} disabled={!draft.name.trim() || Boolean(pending)} style={primaryButton(!draft.name.trim() || Boolean(pending))}>
              {pending === '__save__' ? 'Saving…' : 'Save'}
            </button>
          </div>
        </div>
      </PanelBox>

      {error && <Notice tone="error">{error}</Notice>}
      {preview && (
        <Notice tone="ok">
          {preview.name}: {preview.count.toLocaleString()} matching torrent{preview.count === 1 ? '' : 's'}
        </Notice>
      )}

      <div style={{ ...scrollX, display: 'grid', gap: 8, maxWidth: 980 }}>
        {isLoading && <SkeletonRows count={3} />}
        {!isLoading && groups.length === 0 && (
          <EmptyState title="No ratio groups configured" detail="Create a group above, then preview which torrents match before applying it." />
        )}
        {groups.map(group => (
          <div key={group.name} className="tng-automation-row" data-enabled={group.enabled ? 'true' : 'false'} style={{
            display: 'grid', gridTemplateColumns: '160px 90px 110px 150px minmax(220px, 1fr) auto auto auto',
            minWidth: 900,
            gap: 8, alignItems: 'center', border: '1px solid var(--border)',
            borderRadius: 7, padding: '10px 12px', background: 'var(--surface)', fontSize: 12,
          }}>
            <strong style={{ color: 'var(--text)', display: 'inline-flex', alignItems: 'center', gap: 7 }}>
              <span aria-hidden="true" style={{
                width: 7, height: 7, borderRadius: 999,
                background: group.enabled ? 'var(--success)' : 'var(--faint)',
              }} />
              {group.name}
            </strong>
            <span style={{ color: 'var(--muted)' }}>ratio {group.ratio_limit}</span>
            <span style={{ color: 'var(--muted)' }}>{group.seeding_time_limit < 0 ? 'no time cap' : `${group.seeding_time_limit}m`}</span>
            <span style={{ color: 'var(--faint)' }}>{group.category || 'any category'}</span>
            <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{group.tracker || 'any tracker'}</span>
            <button
              onClick={() => apply(group.name, true)}
              disabled={Boolean(pending)}
              style={{
                background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
                color: 'var(--muted)', padding: '3px 8px', fontSize: 11,
                cursor: pending ? 'not-allowed' : 'pointer',
                opacity: pending ? 0.55 : 1,
              }}
            >
              Preview
            </button>
            <button
              onClick={() => apply(group.name, false)}
              disabled={Boolean(pending)}
              style={{
                background: 'var(--surface-2)', border: '1px solid var(--accent)', borderRadius: 4,
                color: 'var(--accent)', padding: '3px 8px', fontSize: 11,
                cursor: pending ? 'not-allowed' : 'pointer',
                opacity: pending ? 0.55 : 1,
              }}
            >
              Apply
            </button>
            <button
              onClick={() => remove(group.name)}
              disabled={Boolean(pending)}
              style={{
                background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
                color: 'var(--faint)', padding: '3px 8px', fontSize: 11,
                cursor: pending ? 'not-allowed' : 'pointer',
                opacity: pending ? 0.55 : 1,
              }}
            >
              Delete
            </button>
          </div>
        ))}
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

function PanelBox({ children }: { children: React.ReactNode }) {
  return <div style={{
    maxWidth: 1020,
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
      <span className="tng-skeleton" style={{ width: '32%', height: 12, marginBottom: 10 }} />
      <span className="tng-skeleton" style={{ width: '76%', height: 10 }} />
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

const scrollX: React.CSSProperties = {
  overflowX: 'auto',
  paddingBottom: 2,
}

function Input({ value, placeholder, onChange }: { value: string; placeholder: string; onChange: (value: string) => void }) {
  return (
    <input
      value={value}
      placeholder={placeholder}
      onChange={e => onChange(e.target.value)}
      style={{
        minWidth: 0, background: 'var(--bg)', border: '1px solid var(--border-strong)',
        borderRadius: 5, color: 'var(--text)', padding: '4px 8px', fontSize: 12,
      }}
    />
  )
}
