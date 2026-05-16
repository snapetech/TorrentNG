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

  const { data: groups = [] } = useQuery({
    queryKey: ['ratio-groups'],
    queryFn: api.ratioGroups.list,
    staleTime: 30_000,
  })

  async function save() {
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
    }
  }

  async function remove(name: string) {
    await api.ratioGroups.delete(name)
    qc.invalidateQueries({ queryKey: ['ratio-groups'] })
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
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 12, color: '#e2e8f0' }}>
        Ratio Groups
      </div>

      <div style={{ display: 'grid', gridTemplateColumns: '160px 90px 110px 150px 1fr auto', gap: 8, maxWidth: 980, marginBottom: 12 }}>
        <Input value={draft.name} placeholder="Name" onChange={name => setDraft({ ...draft, name })} />
        <Input value={String(draft.ratio_limit)} placeholder="Ratio" onChange={value => setDraft({ ...draft, ratio_limit: Number(value) })} />
        <Input value={String(draft.seeding_time_limit)} placeholder="Minutes" onChange={value => setDraft({ ...draft, seeding_time_limit: Number(value) })} />
        <Input value={draft.category ?? ''} placeholder="Category" onChange={category => setDraft({ ...draft, category })} />
        <Input value={draft.tracker ?? ''} placeholder="Tracker contains" onChange={tracker => setDraft({ ...draft, tracker })} />
        <button
          onClick={save}
          disabled={!draft.name.trim()}
          style={{
            background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
            color: '#93c5fd', padding: '4px 10px', fontSize: 12,
            cursor: draft.name.trim() ? 'pointer' : 'not-allowed',
            opacity: draft.name.trim() ? 1 : 0.5,
          }}
        >
          Save
        </button>
      </div>

      {error && <div style={{ color: '#ef4444', fontSize: 12, marginBottom: 10 }}>{error}</div>}
      {preview && (
        <div style={{ color: '#94a3b8', fontSize: 12, marginBottom: 10 }}>
          {preview.name}: {preview.count.toLocaleString()} matching torrent{preview.count === 1 ? '' : 's'}
        </div>
      )}

      <div style={{ display: 'grid', gap: 8, maxWidth: 980 }}>
        {groups.map(group => (
          <div key={group.name} style={{
            display: 'grid', gridTemplateColumns: '160px 90px 110px 150px 1fr auto auto auto',
            gap: 8, alignItems: 'center', border: '1px solid #1e2433',
            borderRadius: 6, padding: '9px 12px', background: '#111827', fontSize: 12,
          }}>
            <strong style={{ color: '#cbd5e1' }}>{group.name}</strong>
            <span style={{ color: '#94a3b8' }}>ratio {group.ratio_limit}</span>
            <span style={{ color: '#94a3b8' }}>{group.seeding_time_limit < 0 ? 'no time cap' : `${group.seeding_time_limit}m`}</span>
            <span style={{ color: '#64748b' }}>{group.category || 'any category'}</span>
            <span style={{ color: '#64748b', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{group.tracker || 'any tracker'}</span>
            <button
              onClick={() => apply(group.name, true)}
              disabled={pending === group.name}
              style={{
                background: 'none', border: '1px solid #334155', borderRadius: 4,
                color: '#94a3b8', padding: '3px 8px', fontSize: 11,
                cursor: pending === group.name ? 'not-allowed' : 'pointer',
              }}
            >
              Preview
            </button>
            <button
              onClick={() => apply(group.name, false)}
              disabled={pending === group.name}
              style={{
                background: '#1e2433', border: '1px solid #3b82f640', borderRadius: 4,
                color: '#3b82f6', padding: '3px 8px', fontSize: 11,
                cursor: pending === group.name ? 'not-allowed' : 'pointer',
              }}
            >
              Apply
            </button>
            <button
              onClick={() => remove(group.name)}
              disabled={pending === group.name}
              style={{
                background: 'none', border: '1px solid #334155', borderRadius: 4,
                color: '#64748b', padding: '3px 8px', fontSize: 11,
                cursor: pending === group.name ? 'not-allowed' : 'pointer',
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

function Input({ value, placeholder, onChange }: { value: string; placeholder: string; onChange: (value: string) => void }) {
  return (
    <input
      value={value}
      placeholder={placeholder}
      onChange={e => onChange(e.target.value)}
      style={{
        minWidth: 0, background: '#0f1117', border: '1px solid #334155',
        borderRadius: 5, color: '#cbd5e1', padding: '4px 8px', fontSize: 12,
      }}
    />
  )
}
