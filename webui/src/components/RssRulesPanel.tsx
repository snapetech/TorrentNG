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
  const [pending, setPending] = useState<string | null>(null)
  const { data: rules = [], isLoading } = useQuery({
    queryKey: ['rss-rules'],
    queryFn: api.rssRules.list,
    staleTime: 30_000,
  })

  async function save() {
    if (pending) return
    setPending('__save__')
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
    } finally {
      setPending(null)
    }
  }

  async function remove(id: string) {
    setPending(id)
    setError(null)
    try {
      await api.rssRules.delete(id)
      qc.invalidateQueries({ queryKey: ['rss-rules'] })
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  async function test() {
    if (pending) return
    setPending('__test__')
    setError(null)
    setApplyResult(null)
    try {
      const result = await api.rssRules.test(sampleTitle.trim(), sampleLink.trim())
      setMatches(result.matches)
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  async function apply(dryRun: boolean) {
    if (pending) return
    setPending(dryRun ? '__preview__' : '__apply__')
    setError(null)
    setApplyResult(null)
    try {
      const result = await api.rssRules.apply(sampleTitle.trim(), sampleLink.trim(), dryRun)
      setApplyResult(`${result.dry_run ? 'Preview' : 'Apply'}: ${result.applied.length} matched, ${result.errors.length} error(s)`)
    } catch (e) {
      setError(String(e))
    } finally {
      setPending(null)
    }
  }

  return (
    <section style={{ padding: '18px 24px' }}>
      <div style={{ fontSize: 14, fontWeight: 600, marginBottom: 12, color: 'var(--text)' }}>
        RSS Rules
      </div>
      <div style={scrollX}>
        <div style={{ display: 'grid', gridTemplateColumns: '130px minmax(240px, 1fr) 130px 120px 120px auto', gap: 8, minWidth: 850, maxWidth: 1080, marginBottom: 8 }}>
          <Input value={draft.name} placeholder="Name" onChange={name => setDraft({ ...draft, name })} />
          <Input value={draft.feed_url} placeholder="Feed URL" onChange={feed_url => setDraft({ ...draft, feed_url })} />
          <Input value={draft.include} placeholder="Include" onChange={include => setDraft({ ...draft, include })} />
          <Input value={draft.exclude ?? ''} placeholder="Exclude" onChange={exclude => setDraft({ ...draft, exclude })} />
          <Input value={draft.category ?? ''} placeholder="Category" onChange={category => setDraft({ ...draft, category })} />
          <button
            onClick={save}
            disabled={!draft.name.trim() || !draft.feed_url.trim() || !draft.include.trim() || Boolean(pending)}
            style={buttonStyle(!draft.name.trim() || !draft.feed_url.trim() || !draft.include.trim() || Boolean(pending))}
          >
            {pending === '__save__' ? 'Saving…' : 'Save'}
          </button>
        </div>
        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(240px, 1fr) 180px 180px auto', gap: 8, minWidth: 680, maxWidth: 1080, marginBottom: 12 }}>
          <Input value={draft.save_path ?? ''} placeholder="Save path" onChange={save_path => setDraft({ ...draft, save_path })} />
          <Input value={draft.tags.join(',')} placeholder="Tags" onChange={tags => setDraft({ ...draft, tags: tags.split(',') })} />
          <label style={{ display: 'flex', alignItems: 'center', gap: 6, color: 'var(--muted)', fontSize: 12 }}>
            <input type="checkbox" checked={draft.start} onChange={e => setDraft({ ...draft, start: e.target.checked })} />
            Start
          </label>
        </div>
      </div>
      {error && <div style={{ color: '#ef4444', fontSize: 12, marginBottom: 10 }}>{error}</div>}

      <div style={{ ...scrollX, display: 'grid', gap: 8, maxWidth: 1080 }}>
        {isLoading && <div style={{ color: 'var(--faint)', fontSize: 12 }}>Loading RSS rules…</div>}
        {!isLoading && rules.length === 0 && (
          <div style={{ color: 'var(--faint)', fontSize: 12, padding: '8px 0' }}>No RSS rules configured.</div>
        )}
        {rules.map(rule => (
          <div key={rule.id} style={{
            display: 'grid', gridTemplateColumns: '140px minmax(260px, 1fr) 120px 120px 120px auto',
            minWidth: 860,
            gap: 8, alignItems: 'center', border: '1px solid var(--border)',
            borderRadius: 6, padding: '9px 12px', background: 'var(--surface)', fontSize: 12,
          }}>
            <strong style={{ color: 'var(--text)' }}>{rule.name}</strong>
            <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{rule.feed_url}</span>
            <span style={{ color: 'var(--muted)' }}>{rule.include}</span>
            <span style={{ color: 'var(--faint)' }}>{rule.category || 'no category'}</span>
            <span style={{ color: rule.enabled ? '#22c55e' : 'var(--faint)' }}>{rule.enabled ? 'enabled' : 'disabled'}</span>
            <button onClick={() => remove(rule.id)} disabled={Boolean(pending)} style={ghostButtonStyle(Boolean(pending))}>
              {pending === rule.id ? 'Deleting…' : 'Delete'}
            </button>
          </div>
        ))}
      </div>

      <div style={{ fontSize: 13, fontWeight: 600, marginTop: 20, marginBottom: 10, color: 'var(--text)' }}>
        Match Test
      </div>
      <div style={scrollX}>
        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(220px, 1fr) minmax(220px, 1fr) auto auto auto', gap: 8, minWidth: 720, maxWidth: 1080, marginBottom: 8 }}>
          <Input value={sampleTitle} placeholder="Sample title" onChange={setSampleTitle} />
          <Input value={sampleLink} placeholder="Sample link" onChange={setSampleLink} />
          <button onClick={test} disabled={!sampleTitle.trim() || Boolean(pending)} style={buttonStyle(!sampleTitle.trim() || Boolean(pending))}>
            {pending === '__test__' ? 'Testing…' : 'Test'}
          </button>
          <button onClick={() => apply(true)} disabled={!sampleTitle.trim() || !sampleLink.trim() || Boolean(pending)} style={buttonStyle(!sampleTitle.trim() || !sampleLink.trim() || Boolean(pending))}>
            {pending === '__preview__' ? 'Previewing…' : 'Preview'}
          </button>
          <button onClick={() => apply(false)} disabled={!sampleTitle.trim() || !sampleLink.trim() || Boolean(pending)} style={buttonStyle(!sampleTitle.trim() || !sampleLink.trim() || Boolean(pending))}>
            {pending === '__apply__' ? 'Applying…' : 'Apply'}
          </button>
        </div>
      </div>
      {applyResult && <div style={{ color: 'var(--muted)', fontSize: 12, marginBottom: 8 }}>{applyResult}</div>}
      <div style={{ display: 'grid', gap: 6, maxWidth: 1080 }}>
        {matches.map(match => (
          <div key={match.rule_id} style={{ color: match.matched ? '#22c55e' : 'var(--faint)', fontSize: 12 }}>
            {match.rule_name}: {match.reason}
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

function buttonStyle(disabled = false): React.CSSProperties {
  return {
  background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
  color: 'var(--accent-text)', padding: '4px 10px', fontSize: 12,
  cursor: disabled ? 'not-allowed' : 'pointer', opacity: disabled ? 0.55 : 1,
  }
}

function ghostButtonStyle(disabled = false): React.CSSProperties {
  return {
  background: 'none', border: '1px solid var(--border-strong)', borderRadius: 4,
  color: 'var(--faint)', padding: '3px 8px', fontSize: 11,
  cursor: disabled ? 'not-allowed' : 'pointer', opacity: disabled ? 0.55 : 1,
  }
}

function Input({ value, placeholder, onChange }: { value: string; placeholder: string; onChange: (value: string) => void }) {
  return <input value={value} placeholder={placeholder} onChange={e => onChange(e.target.value)} style={{
    minWidth: 0, background: 'var(--bg)', border: '1px solid var(--border-strong)',
    borderRadius: 5, color: 'var(--text)', padding: '4px 8px', fontSize: 12,
  }} />
}
