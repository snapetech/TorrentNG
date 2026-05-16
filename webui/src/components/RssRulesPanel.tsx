import { useState } from 'react'
import { useQuery, useQueryClient } from '@tanstack/react-query'
import { api } from '../api/client'
import type { RssRule, RssRuleMatch } from '../api/client'

const EMPTY: RssRule = {
  id: '',
  name: '',
  enabled: true,
  feed_url: '',
  include: '',
  exclude: null,
  category: null,
  save_path: null,
  tags: [],
  start: true,
}

export function RssRulesPanel() {
  const qc = useQueryClient()
  const [draft, setDraft] = useState<RssRule>(EMPTY)
  const [sampleTitle, setSampleTitle] = useState('')
  const [sampleLink, setSampleLink] = useState('')
  const [matches, setMatches] = useState<RssRuleMatch[]>([])
  const [applyResult, setApplyResult] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const { data: rules = [] } = useQuery({
    queryKey: ['rss-rules'],
    queryFn: api.rssRules.list,
    staleTime: 30_000,
  })

  async function save() {
    setError(null)
    try {
      await api.rssRules.save({
        ...draft,
        name: draft.name.trim(),
        feed_url: draft.feed_url.trim(),
        include: draft.include.trim(),
        exclude: draft.exclude?.trim() || null,
        category: draft.category?.trim() || null,
        save_path: draft.save_path?.trim() || null,
        tags: draft.tags.map(tag => tag.trim()).filter(Boolean),
      })
      setDraft(EMPTY)
      qc.invalidateQueries({ queryKey: ['rss-rules'] })
    } catch (e) {
      setError(String(e))
    }
  }

  async function remove(id: string) {
    await api.rssRules.delete(id)
    qc.invalidateQueries({ queryKey: ['rss-rules'] })
  }

  async function test() {
    setError(null)
    setApplyResult(null)
    try {
      const result = await api.rssRules.test(sampleTitle.trim(), sampleLink.trim())
      setMatches(result.matches)
    } catch (e) {
      setError(String(e))
    }
  }

  async function apply(dryRun: boolean) {
    setError(null)
    setApplyResult(null)
    try {
      const result = await api.rssRules.apply(sampleTitle.trim(), sampleLink.trim(), dryRun)
      setApplyResult(`${result.dry_run ? 'Preview' : 'Apply'}: ${result.applied.length} matched, ${result.errors.length} error(s)`)
    } catch (e) {
      setError(String(e))
    }
  }

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 12, color: '#e2e8f0' }}>
        RSS Rules
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '130px 1fr 130px 120px 120px auto', gap: 8, maxWidth: 1080, marginBottom: 8 }}>
        <Input value={draft.name} placeholder="Name" onChange={name => setDraft({ ...draft, name })} />
        <Input value={draft.feed_url} placeholder="Feed URL" onChange={feed_url => setDraft({ ...draft, feed_url })} />
        <Input value={draft.include} placeholder="Include" onChange={include => setDraft({ ...draft, include })} />
        <Input value={draft.exclude ?? ''} placeholder="Exclude" onChange={exclude => setDraft({ ...draft, exclude })} />
        <Input value={draft.category ?? ''} placeholder="Category" onChange={category => setDraft({ ...draft, category })} />
        <button onClick={save} disabled={!draft.name.trim() || !draft.feed_url.trim() || !draft.include.trim()} style={buttonStyle}>
          Save
        </button>
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 180px 180px auto', gap: 8, maxWidth: 1080, marginBottom: 12 }}>
        <Input value={draft.save_path ?? ''} placeholder="Save path" onChange={save_path => setDraft({ ...draft, save_path })} />
        <Input value={draft.tags.join(',')} placeholder="Tags" onChange={tags => setDraft({ ...draft, tags: tags.split(',') })} />
        <label style={{ display: 'flex', alignItems: 'center', gap: 6, color: '#94a3b8', fontSize: 12 }}>
          <input type="checkbox" checked={draft.start} onChange={e => setDraft({ ...draft, start: e.target.checked })} />
          Start
        </label>
      </div>
      {error && <div style={{ color: '#ef4444', fontSize: 12, marginBottom: 10 }}>{error}</div>}

      <div style={{ display: 'grid', gap: 8, maxWidth: 1080 }}>
        {rules.map(rule => (
          <div key={rule.id} style={{
            display: 'grid', gridTemplateColumns: '140px 1fr 120px 120px 120px auto',
            gap: 8, alignItems: 'center', border: '1px solid #1e2433',
            borderRadius: 6, padding: '9px 12px', background: '#111827', fontSize: 12,
          }}>
            <strong style={{ color: '#cbd5e1' }}>{rule.name}</strong>
            <span style={{ color: '#64748b', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{rule.feed_url}</span>
            <span style={{ color: '#94a3b8' }}>{rule.include}</span>
            <span style={{ color: '#64748b' }}>{rule.category || 'no category'}</span>
            <span style={{ color: rule.enabled ? '#22c55e' : '#64748b' }}>{rule.enabled ? 'enabled' : 'disabled'}</span>
            <button onClick={() => remove(rule.id)} style={ghostButtonStyle}>Delete</button>
          </div>
        ))}
      </div>

      <div style={{ fontSize: 13, fontWeight: 600, marginTop: 20, marginBottom: 10, color: '#e2e8f0' }}>
        Match Test
      </div>
      <div style={{ display: 'grid', gridTemplateColumns: '1fr 1fr auto auto auto', gap: 8, maxWidth: 1080, marginBottom: 8 }}>
        <Input value={sampleTitle} placeholder="Sample title" onChange={setSampleTitle} />
        <Input value={sampleLink} placeholder="Sample link" onChange={setSampleLink} />
        <button onClick={test} disabled={!sampleTitle.trim()} style={buttonStyle}>Test</button>
        <button onClick={() => apply(true)} disabled={!sampleTitle.trim() || !sampleLink.trim()} style={buttonStyle}>Preview</button>
        <button onClick={() => apply(false)} disabled={!sampleTitle.trim() || !sampleLink.trim()} style={buttonStyle}>Apply</button>
      </div>
      {applyResult && <div style={{ color: '#94a3b8', fontSize: 12, marginBottom: 8 }}>{applyResult}</div>}
      <div style={{ display: 'grid', gap: 6, maxWidth: 1080 }}>
        {matches.map(match => (
          <div key={match.rule_id} style={{ color: match.matched ? '#22c55e' : '#64748b', fontSize: 12 }}>
            {match.rule_name}: {match.reason}
          </div>
        ))}
      </div>
    </section>
  )
}

const buttonStyle = {
  background: '#1e3a5f', border: '1px solid #3b82f6', borderRadius: 5,
  color: '#93c5fd', padding: '4px 10px', fontSize: 12, cursor: 'pointer',
}

const ghostButtonStyle = {
  background: 'none', border: '1px solid #334155', borderRadius: 4,
  color: '#64748b', padding: '3px 8px', fontSize: 11, cursor: 'pointer',
}

function Input({ value, placeholder, onChange }: { value: string; placeholder: string; onChange: (value: string) => void }) {
  return <input value={value} placeholder={placeholder} onChange={e => onChange(e.target.value)} style={{
    minWidth: 0, background: '#0f1117', border: '1px solid #334155',
    borderRadius: 5, color: '#cbd5e1', padding: '4px 8px', fontSize: 12,
  }} />
}
