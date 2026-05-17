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
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 12, color: 'var(--text)' }}>
        Ratio Groups
      </div>

      <div style={scrollX}>
        <div style={{ display: 'grid', gridTemplateColumns: '160px 90px 110px 150px minmax(220px, 1fr) auto', gap: 8, minWidth: 820, maxWidth: 980, marginBottom: 12 }}>
          <Input value={draft.name} placeholder="Name" onChange={name => setDraft({ ...draft, name })} />
          <Input value={String(draft.ratio_limit)} placeholder="Ratio" onChange={value => setDraft({ ...draft, ratio_limit: Number(value) })} />
          <Input value={String(draft.seeding_time_limit)} placeholder="Minutes" onChange={value => setDraft({ ...draft, seeding_time_limit: Number(value) })} />
          <Input value={draft.category ?? ''} placeholder="Category" onChange={category => setDraft({ ...draft, category })} />
          <Input value={draft.tracker ?? ''} placeholder="Tracker contains" onChange={tracker => setDraft({ ...draft, tracker })} />
          <button
            onClick={save}
            disabled={!draft.name.trim() || Boolean(pending)}
            style={{
              background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
              color: 'var(--accent-text)', padding: '4px 10px', fontSize: 12,
              cursor: draft.name.trim() && !pending ? 'pointer' : 'not-allowed',
              opacity: draft.name.trim() && !pending ? 1 : 0.5,
            }}
          >
            {pending === '__save__' ? 'Saving…' : 'Save'}
          </button>
        </div>
      </div>

      {error && <div style={{ color: '#ef4444', fontSize: 12, marginBottom: 10 }}>{error}</div>}
      {preview && (
        <div style={{ color: 'var(--muted)', fontSize: 12, marginBottom: 10 }}>
          {preview.name}: {preview.count.toLocaleString()} matching torrent{preview.count === 1 ? '' : 's'}
        </div>
      )}

      <div style={{ ...scrollX, display: 'grid', gap: 8, maxWidth: 980 }}>
        {isLoading && <div style={{ color: 'var(--faint)', fontSize: 12 }}>Loading ratio groups…</div>}
        {!isLoading && groups.length === 0 && (
          <div style={{ color: 'var(--faint)', fontSize: 12, padding: '8px 0' }}>No ratio groups configured.</div>
        )}
        {groups.map(group => (
          <div key={group.name} style={{
            display: 'grid', gridTemplateColumns: '160px 90px 110px 150px minmax(220px, 1fr) auto auto auto',
            minWidth: 900,
            gap: 8, alignItems: 'center', border: '1px solid var(--border)',
            borderRadius: 6, padding: '9px 12px', background: 'var(--surface)', fontSize: 12,
          }}>
            <strong style={{ color: 'var(--text)' }}>{group.name}</strong>
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
