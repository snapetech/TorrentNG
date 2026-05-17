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
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
        <div>
          <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)' }}>RSS Rules</div>
          <div style={{ fontSize: 12, color: 'var(--faint)', marginTop: 2 }}>
            {rules.length.toLocaleString()} configured · {matches.length.toLocaleString()} match results
          </div>
        </div>
        {pending && <Busy label={pending.replace(/^__|__$/g, '') || 'Working'} />}
      </div>
      <PanelBox>
        <div style={{ fontSize: 12, fontWeight: 800, color: 'var(--text)', marginBottom: 10 }}>Rule builder</div>
        <div style={scrollX}>
          <div style={{ display: 'grid', gridTemplateColumns: '130px minmax(240px, 1fr) 130px 120px 120px auto', gap: 8, minWidth: 850, maxWidth: 1080, marginBottom: 10 }}>
            <Field label="Name"><Input value={draft.name} placeholder="new releases" onChange={name => setDraft({ ...draft, name })} /></Field>
            <Field label="Feed URL"><Input value={draft.feed_url} placeholder="https://..." onChange={feed_url => setDraft({ ...draft, feed_url })} /></Field>
            <Field label="Include"><Input value={draft.include} placeholder="required text" onChange={include => setDraft({ ...draft, include })} /></Field>
            <Field label="Exclude"><Input value={draft.exclude ?? ''} placeholder="optional" onChange={exclude => setDraft({ ...draft, exclude })} /></Field>
            <Field label="Category"><Input value={draft.category ?? ''} placeholder="optional" onChange={category => setDraft({ ...draft, category })} /></Field>
            <button
              onClick={save}
              disabled={!draft.name.trim() || !draft.feed_url.trim() || !draft.include.trim() || Boolean(pending)}
              style={buttonStyle(!draft.name.trim() || !draft.feed_url.trim() || !draft.include.trim() || Boolean(pending))}
            >
              {pending === '__save__' ? 'Saving…' : 'Save'}
            </button>
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'minmax(240px, 1fr) 180px 120px', gap: 8, minWidth: 620, maxWidth: 1080 }}>
            <Field label="Save path"><Input value={draft.save_path ?? ''} placeholder="/downloads" onChange={save_path => setDraft({ ...draft, save_path })} /></Field>
            <Field label="Tags"><Input value={draft.tags.join(',')} placeholder="comma,separated" onChange={tags => setDraft({ ...draft, tags: tags.split(',') })} /></Field>
            <label style={{
              alignSelf: 'end', display: 'flex', alignItems: 'center', justifyContent: 'center', gap: 7,
              color: 'var(--muted)', fontSize: 12, border: '1px solid var(--border-strong)', borderRadius: 5,
              minHeight: 29, background: 'var(--bg)',
            }}>
              <input type="checkbox" checked={draft.start} onChange={e => setDraft({ ...draft, start: e.target.checked })} />
              Start
            </label>
          </div>
        </div>
      </PanelBox>
      {error && <Notice tone="error">{error}</Notice>}

      <div style={{ ...scrollX, display: 'grid', gap: 8, maxWidth: 1080 }}>
        {isLoading && <SkeletonRows count={3} />}
        {!isLoading && rules.length === 0 && (
          <EmptyState title="No RSS rules configured" detail="Create a rule above, then use match test to check titles before applying." />
        )}
        {rules.map(rule => (
          <div key={rule.id} style={{
            display: 'grid', gridTemplateColumns: '140px minmax(260px, 1fr) 120px 120px 120px auto',
            minWidth: 860,
            gap: 8, alignItems: 'center', border: '1px solid var(--border)',
            borderRadius: 7, padding: '10px 12px', background: 'var(--surface)', fontSize: 12,
          }}>
            <strong style={{ color: 'var(--text)' }}>{rule.name}</strong>
            <span style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{rule.feed_url}</span>
            <span style={{ color: 'var(--muted)' }}>{rule.include}</span>
            <span style={{ color: 'var(--faint)' }}>{rule.category || 'no category'}</span>
            <Pill tone={rule.enabled ? 'ok' : 'idle'}>{rule.enabled ? 'enabled' : 'disabled'}</Pill>
            <button onClick={() => remove(rule.id)} disabled={Boolean(pending)} style={ghostButtonStyle(Boolean(pending))}>
              {pending === rule.id ? 'Deleting…' : 'Delete'}
            </button>
          </div>
        ))}
      </div>

      <div style={{ fontSize: 13, fontWeight: 600, marginTop: 20, marginBottom: 10, color: 'var(--text)' }}>
        Match Test
      </div>
      <PanelBox>
        <div style={{ fontSize: 12, fontWeight: 800, color: 'var(--text)', marginBottom: 10 }}>Sample item</div>
        <div style={scrollX}>
        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(220px, 1fr) minmax(220px, 1fr) auto auto auto', gap: 8, minWidth: 720, maxWidth: 1080, marginBottom: 8 }}>
          <Field label="Title"><Input value={sampleTitle} placeholder="Sample title" onChange={setSampleTitle} /></Field>
          <Field label="Link"><Input value={sampleLink} placeholder="https://..." onChange={setSampleLink} /></Field>
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
      </PanelBox>
      {applyResult && <Notice tone="ok">{applyResult}</Notice>}
      <div style={{ display: 'grid', gap: 6, maxWidth: 1080 }}>
        {matches.map(match => (
          <div key={match.rule_id} style={{
            display: 'flex', alignItems: 'center', gap: 8, border: '1px solid var(--border)',
            borderRadius: 6, padding: '8px 10px', background: 'var(--surface)', fontSize: 12,
          }}>
            <Pill tone={match.matched ? 'ok' : 'idle'}>{match.matched ? 'match' : 'skip'}</Pill>
            <span style={{ color: 'var(--text)', fontWeight: 700 }}>{match.rule_name}</span>
            <span style={{ color: 'var(--faint)' }}>{match.reason}</span>
          </div>
        ))}
      </div>
    </section>
  )
}

function Busy({ label }: { label: string }) {
  return <span style={{
    color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
    borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 700, textTransform: 'capitalize',
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

function Pill({ tone, children }: { tone: 'ok' | 'idle'; children: React.ReactNode }) {
  return <span style={{
    justifySelf: 'start',
    color: tone === 'ok' ? 'var(--success)' : 'var(--faint)',
    border: '1px solid ' + (tone === 'ok' ? 'color-mix(in srgb, var(--success) 45%, var(--border))' : 'var(--border-strong)'),
    background: tone === 'ok' ? 'color-mix(in srgb, var(--success) 8%, transparent)' : 'var(--surface-2)',
    borderRadius: 999,
    padding: '2px 8px',
    fontSize: 11,
    fontWeight: 800,
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
      <span className="rtng-skeleton" style={{ width: '28%', height: 12, marginBottom: 10 }} />
      <span className="rtng-skeleton" style={{ width: '82%', height: 10 }} />
    </div>
  ))
}

function buttonStyle(disabled = false): React.CSSProperties {
  return {
  background: 'var(--accent-soft)', border: '1px solid var(--accent)', borderRadius: 5,
  color: 'var(--accent-text)', padding: '5px 12px', fontSize: 12, fontWeight: 700, alignSelf: 'end',
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
