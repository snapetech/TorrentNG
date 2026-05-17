import { useEffect, useMemo, useState } from 'react'
import { useMutation, useQuery, useQueryClient } from '@tanstack/react-query'
import { api, type EngineDiagnostics, type ProbeValue } from '../api/client'

export function EnginePanel() {
  const { data, isLoading, isFetching, error, refetch } = useQuery({
    queryKey: ['engine'],
    queryFn: api.engine,
    staleTime: 2_000,
    refetchInterval: 5_000,
  })
  const { data: commands, refetch: refetchCommands } = useQuery({
    queryKey: ['engine-commands'],
    queryFn: api.engineCommands,
    refetchInterval: 60000,
  })

  const driftProblems = data?.drift.filter(row => row.status !== 'match').length ?? 0

  return (
    <section style={{ padding: '16px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
        <h2 style={{ fontSize: 13, margin: 0, color: 'var(--text)' }}>Engine</h2>
        {data && (
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <Badge ok={driftProblems === 0} text={driftProblems === 0 ? 'profile clean' : `${driftProblems} drift`} />
            <Badge ok={data.capabilities.every(c => c.available)} text={`${data.capabilities.filter(c => c.available).length}/${data.capabilities.length} capabilities`} />
            <button
              onClick={() => {
                refetch()
                refetchCommands()
              }}
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
        )}
      </div>

      {isLoading && <EngineSkeleton />}
      {error && <Notice>Engine diagnostics unavailable</Notice>}
      {data && (
        <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(320px, 1fr))', gap: 16 }}>
          <Provenance data={data} />
          <Capabilities data={data} />
          <HttpStack data={data} />
          <DhtStack data={data} />
          <RtorrentSettingsPanel />
          <ProfileDrift data={data} />
          <CommandIndex commands={commands} />
        </div>
      )}
    </section>
  )
}

function EngineSkeleton() {
  return (
    <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(320px, 1fr))', gap: 16 }}>
      {Array.from({ length: 4 }).map((_, index) => (
        <Panel key={index}>
          <span className="tng-skeleton" style={{ width: 120, height: 12, marginBottom: 12 }} />
          <span className="tng-skeleton" style={{ width: '92%', height: 10, marginBottom: 8 }} />
          <span className="tng-skeleton" style={{ width: '72%', height: 10, marginBottom: 8 }} />
          <span className="tng-skeleton" style={{ width: '84%', height: 10 }} />
        </Panel>
      ))}
    </div>
  )
}

function Notice({ children }: { children: React.ReactNode }) {
  return (
    <div style={{
      color: 'var(--danger)', background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
      border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
      borderRadius: 6, padding: '8px 9px', fontSize: 12,
    }}>{children}</div>
  )
}

function Provenance({ data }: { data: EngineDiagnostics }) {
  const p = data.provenance
  return (
    <Panel>
      <Subhead>Provenance</Subhead>
      <Rows rows={[
        ['Sidecar', p.sidecar_version],
        ['rTorrent', p.rtorrent_version ?? 'unknown'],
        ['libtorrent', p.libtorrent_version ?? 'unknown'],
        ['XMLRPC', p.xmlrpc_backend],
        ['Packaged rTorrent', p.packaged_rtorrent_version ?? 'not declared'],
        ['Packaged libtorrent', p.packaged_libtorrent_version ?? 'not declared'],
        ['Patches', p.patch_set.length ? p.patch_set.join(', ') : 'none declared'],
      ]} />
    </Panel>
  )
}

function Capabilities({ data }: { data: EngineDiagnostics }) {
  return (
    <Panel>
      <Subhead>Capabilities</Subhead>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(210px, 1fr))', gap: 8 }}>
        {data.capabilities.map(cap => (
          <div key={cap.key} className="tng-engine-capability" data-available={cap.available ? 'true' : 'false'} style={{
            border: '1px solid var(--border)',
            borderRadius: 6,
            padding: '8px 10px',
            background: cap.available ? 'var(--surface)' : 'color-mix(in srgb, var(--danger) 10%, var(--surface))',
            minWidth: 0,
          }}>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
              <span style={{ fontSize: 12, color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{cap.label}</span>
              <Badge ok={cap.available} text={cap.available ? 'yes' : 'no'} />
            </div>
            <div title={cap.command} style={{ fontSize: 11, color: 'var(--faint)', marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {cap.command}
            </div>
          </div>
        ))}
      </div>
    </Panel>
  )
}

function HttpStack({ data }: { data: EngineDiagnostics }) {
  const h = data.http
  return (
    <Panel wide>
      <Subhead>Tracker HTTP Stack</Subhead>
      <Rows rows={[
        ['User agent', val(h.user_agent)],
        ['Open requests', val(h.current_open)],
        ['Max total', val(h.max_total_connections)],
        ['Max per host', val(h.max_host_connections)],
        ['Max cached', val(h.max_cache_connections)],
        ['DNS cache timeout', suffix(h.dns_cache_timeout, 's')],
        ['Proxy', val(h.proxy_address) || 'none'],
        ['CA path', val(h.ca_path) || 'default'],
        ['CA cert', val(h.ca_cert) || 'default'],
        ['Verify peer', bool(h.ssl_verify_peer)],
        ['Verify host', bool(h.ssl_verify_host)],
      ]} />
    </Panel>
  )
}

function DhtStack({ data }: { data: EngineDiagnostics }) {
  const d = data.dht
  return (
    <Panel wide>
      <Subhead>DHT And Peer Discovery</Subhead>
      <Rows rows={[
        ['DHT', val(d.enabled) || 'unknown'],
        ['DHT port', val(d.port)],
        ['DHT override port', val(d.override_port) || 'none'],
        ['Listen port', val(d.listen_port)],
        ['Listen range', val(d.listen_range)],
        ['PEX', bool(d.pex)],
        ['UDP trackers', bool(d.udp_trackers)],
        ['DHT statistics', val(d.statistics) || 'unavailable'],
      ]} />
    </Panel>
  )
}

function ProfileDrift({ data }: { data: EngineDiagnostics }) {
  const rows = data.drift
  const problems = rows.filter(row => row.status !== 'match')
  return (
    <Panel wide>
      <Subhead>Engine Profile Drift</Subhead>
      {problems.length === 0 && <div style={{ color: 'var(--success)', fontSize: 12 }}>Running profile matches TorrentNG defaults</div>}
      {problems.length > 0 && (
        <div style={{ display: 'grid', gap: 6 }}>
          {problems.map(row => (
            <div key={row.key} className="tng-engine-drift" data-status={row.status} style={{
              display: 'grid',
              gridTemplateColumns: '180px minmax(0, 1fr) minmax(0, 1fr)',
              gap: 10,
              alignItems: 'center',
              border: '1px solid color-mix(in srgb, var(--danger) 50%, var(--border))',
              borderRadius: 6,
              padding: '7px 9px',
              background: 'color-mix(in srgb, var(--danger) 10%, var(--surface))',
              fontSize: 12,
            }}>
              <div title={row.command} style={{ color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{row.label}</div>
              <div title={row.expected} style={{ color: 'var(--faint)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>Expected {row.expected}</div>
              <div title={row.actual ?? row.detail ?? ''} style={{ color: 'var(--danger)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {row.status === 'unavailable' ? 'Unavailable' : `Actual ${row.actual ?? ''}`}
              </div>
            </div>
          ))}
        </div>
      )}
    </Panel>
  )
}

function RtorrentSettingsPanel() {
  const qc = useQueryClient()
  const { data, isLoading, error } = useQuery({
    queryKey: ['rtorrent-settings'],
    queryFn: api.rtorrentSettings.get,
    staleTime: 2_000,
  })
  const [draft, setDraft] = useState<Record<string, string | number | boolean>>({})
  const [customRc, setCustomRc] = useState('')
  const [notice, setNotice] = useState<{ tone: 'ok' | 'warn' | 'error'; text: string } | null>(null)

  useEffect(() => {
    if (!data) return
    const next: Record<string, string | number | boolean> = {}
    for (const setting of data.settings) {
      const row = data.values.find(value => value.key === setting.key)
      const value = row?.saved ?? row?.live.value ?? setting.default_value
      next[setting.key] = inputValue(setting.value_type, value)
    }
    setDraft(next)
    setCustomRc(data.custom_rc)
  }, [data])

  const save = useMutation({
    mutationFn: () => api.rtorrentSettings.save(draft, customRc, true),
    onSuccess: result => {
      qc.invalidateQueries({ queryKey: ['rtorrent-settings'] })
      qc.invalidateQueries({ queryKey: ['engine'] })
      const bits = [`saved ${result.applied.length} live setting${result.applied.length === 1 ? '' : 's'}`]
      if (result.restart_required) bits.push('restart required')
      if (result.errors.length) bits.push(`${result.errors.length} live apply error${result.errors.length === 1 ? '' : 's'}`)
      setNotice({ tone: result.errors.length ? 'warn' : 'ok', text: bits.join(' · ') })
    },
    onError: e => setNotice({ tone: 'error', text: String(e) }),
  })
  const restart = useMutation({
    mutationFn: api.rtorrentSettings.restart,
    onSuccess: () => setNotice({ tone: 'warn', text: 'Restart requested. The container/service should come back automatically.' }),
    onError: e => setNotice({ tone: 'error', text: String(e) }),
  })

  return (
    <Panel wide>
      <Subhead>rTorrent Limits</Subhead>
      {isLoading && <span className="tng-skeleton" style={{ width: '70%', height: 12 }} />}
      {error && <InlineNotice>rTorrent settings unavailable</InlineNotice>}
      {data && (
        <div style={{ display: 'grid', gap: 10 }}>
          <div style={{ color: 'var(--faint)', fontSize: 12 }}>
            Saved to <span style={{ color: 'var(--muted)', fontFamily: 'monospace' }}>{data.overlay_path}</span>.
            Live-safe values are applied immediately; port/file/socket and custom lines need a daemon restart.
          </div>
          <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(230px, 1fr))', gap: 8 }}>
            {data.settings.map(setting => {
              const row = data.values.find(value => value.key === setting.key)
              const value = draft[setting.key] ?? inputValue(setting.value_type, row?.saved ?? row?.live.value ?? setting.default_value)
              return (
                <label key={setting.key} className="tng-form-card" style={{
                  border: '1px solid var(--border)', borderRadius: 7, background: 'var(--surface)', padding: 9,
                  display: 'grid', gap: 5,
                }}>
                  <span style={{ display: 'flex', justifyContent: 'space-between', gap: 8, color: 'var(--text)', fontSize: 12, fontWeight: 800 }}>
                    <span>{setting.label}</span>
                    {setting.restart_required && <span style={{ color: 'var(--warning)', fontSize: 10 }}>restart</span>}
                  </span>
                  {setting.value_type === 'bool' ? (
                    <select
                      value={String(Boolean(value))}
                      onChange={e => setDraft(prev => ({ ...prev, [setting.key]: e.target.value === 'true' }))}
                      style={inputStyle}
                    >
                      <option value="true">On</option>
                      <option value="false">Off</option>
                    </select>
                  ) : setting.key === 'dht_mode' ? (
                    <select value={String(value)} onChange={e => setDraft(prev => ({ ...prev, [setting.key]: e.target.value }))} style={inputStyle}>
                      <option value="auto">Auto</option>
                      <option value="on">On</option>
                      <option value="disable">Disabled</option>
                    </select>
                  ) : (
                    <input
                      type="number"
                      min={setting.minimum ?? undefined}
                      max={setting.maximum ?? undefined}
                      value={Number(value)}
                      onChange={e => setDraft(prev => ({ ...prev, [setting.key]: Number(e.target.value) }))}
                      style={inputStyle}
                    />
                  )}
                  <span style={{ color: 'var(--faint)', fontSize: 10, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                    live {row?.live.value ?? 'unavailable'}{setting.unit ? ` ${setting.unit}` : ''} · {setting.command}
                  </span>
                </label>
              )
            })}
          </div>
          <label style={{ display: 'grid', gap: 5 }}>
            <span style={{ color: 'var(--text)', fontSize: 12, fontWeight: 800 }}>Custom rTorrent lines</span>
            <textarea
              value={customRc}
              onChange={e => setCustomRc(e.target.value)}
              rows={4}
              placeholder="Optional advanced rtorrent.rc overrides imported after managed settings"
              style={{ ...inputStyle, resize: 'vertical', fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace' }}
            />
          </label>
          {notice && <div style={{
            color: notice.tone === 'error' ? 'var(--danger)' : notice.tone === 'warn' ? 'var(--warning)' : 'var(--success)',
            fontSize: 12,
          }}>{notice.text}</div>}
          <div style={{ display: 'flex', gap: 8, flexWrap: 'wrap' }}>
            <button onClick={() => save.mutate()} disabled={save.isPending} style={buttonStyle('var(--accent)', 'var(--accent-soft)', 'var(--accent-text)')}>
              {save.isPending ? 'Saving…' : 'Save and apply live'}
            </button>
            <button
              onClick={() => {
                if (window.confirm('Restart rTorrent/TorrentNG now? Active transfers will reconnect after the service comes back.')) restart.mutate()
              }}
              disabled={restart.isPending}
              style={buttonStyle('var(--warning)', 'color-mix(in srgb, var(--warning) 12%, var(--surface))', 'var(--warning)')}
            >
              {restart.isPending ? 'Restarting…' : 'Restart daemon'}
            </button>
          </div>
        </div>
      )}
    </Panel>
  )
}

function inputValue(type: string, value: unknown): string | number | boolean {
  if (type === 'bool') return value === true || value === 'true' || value === 'yes' || value === '1'
  if (typeof value === 'number' || typeof value === 'boolean') return value
  const text = String(value ?? '')
  const numeric = Number(text.replace(/M$/, ''))
  return Number.isFinite(numeric) && type === 'int' ? numeric : text
}

const inputStyle: React.CSSProperties = {
  minWidth: 0,
  width: '100%',
  boxSizing: 'border-box',
  background: 'var(--bg)',
  border: '1px solid var(--border-strong)',
  borderRadius: 5,
  color: 'var(--text)',
  padding: '5px 8px',
  fontSize: 12,
  outline: 'none',
}

function buttonStyle(border: string, background: string, color: string): React.CSSProperties {
  return {
    background,
    border: `1px solid ${border}`,
    borderRadius: 5,
    color,
    padding: '6px 10px',
    fontSize: 12,
    fontWeight: 800,
    cursor: 'pointer',
  }
}

function CommandIndex({ commands }: { commands?: { ok: boolean; count: number; commands: string[]; error: string | null } }) {
  const [filter, setFilter] = useState('')
  const filtered = useMemo(() => {
    const needle = filter.trim().toLowerCase()
    if (!commands?.ok || !needle) return commands?.commands ?? []
    return commands.commands.filter(command => command.toLowerCase().includes(needle))
  }, [commands, filter])
  return (
    <Panel wide>
      <Subhead>XMLRPC Command Surface</Subhead>
      {!commands && <div style={{ display: 'grid', gap: 7 }}>
        <span className="tng-skeleton" style={{ width: '42%', height: 10 }} />
        <span className="tng-skeleton" style={{ width: '72%', height: 8 }} />
      </div>}
      {commands && !commands.ok && <InlineNotice>Command index unavailable</InlineNotice>}
      {commands?.ok && (
        <details>
          <summary style={{ color: 'var(--text)', fontSize: 12, cursor: 'pointer' }}>{commands.count} commands exposed by rTorrent</summary>
          <div style={{ marginTop: 8, display: 'flex', alignItems: 'center', gap: 8 }}>
            <input
              value={filter}
              onChange={e => setFilter(e.target.value)}
              placeholder="Filter commands"
              style={{
                minWidth: 0, flex: '1 1 220px', background: 'var(--bg)', border: '1px solid var(--border-strong)',
                borderRadius: 5, color: 'var(--text)', padding: '5px 8px', fontSize: 12,
              }}
            />
            <span style={{ color: 'var(--faint)', fontSize: 11, whiteSpace: 'nowrap' }}>
              {filtered.length.toLocaleString()} shown
            </span>
          </div>
          <div style={{
            marginTop: 8,
            maxHeight: 180,
            overflow: 'auto',
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))',
            gap: 4,
            fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
            fontSize: 11,
            color: 'var(--muted)',
          }}>
            {filtered.map(command => <div key={command} title={command} style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{command}</div>)}
          </div>
        </details>
      )}
    </Panel>
  )
}

function Panel({ children, wide }: { children: React.ReactNode; wide?: boolean }) {
  return (
    <div className="tng-card tng-engine-panel" style={{
      gridColumn: wide ? '1 / -1' : undefined,
      border: '1px solid var(--border)',
      borderRadius: 7,
      background: 'var(--surface)',
      padding: 12,
      minWidth: 0,
    }}>
      {children}
    </div>
  )
}

function Rows({ rows }: { rows: [string, string][] }) {
  return (
    <div style={{ display: 'grid', gridTemplateColumns: '160px minmax(0, 1fr)', gap: '6px 12px', fontSize: 12 }}>
      {rows.map(([k, v]) => (
        <div key={k} className="tng-engine-kv" style={{ display: 'contents' }}>
          <div style={{ color: 'var(--faint)' }}>{k}</div>
          <div title={v} style={{ color: 'var(--text)', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v}</div>
        </div>
      ))}
    </div>
  )
}

function Subhead({ children }: { children: string }) {
  return <div style={{
    fontSize: 11, fontWeight: 800, textTransform: 'uppercase', color: 'var(--accent-text)',
    marginBottom: 8, display: 'flex', alignItems: 'center', gap: 6,
  }}>
    <span style={{ width: 6, height: 6, borderRadius: 999, background: 'var(--accent)', flexShrink: 0 }} />
    {children}
  </div>
}

function InlineNotice({ children }: { children: React.ReactNode }) {
  return <div style={{
    color: 'var(--danger)',
    background: 'color-mix(in srgb, var(--danger) 9%, var(--surface))',
    border: '1px solid color-mix(in srgb, var(--danger) 45%, var(--border))',
    borderRadius: 6,
    padding: '8px 9px',
    fontSize: 12,
  }}>{children}</div>
}

function Badge({ ok, text }: { ok: boolean; text: string }) {
  return (
    <span style={{
      border: '1px solid ' + (ok ? 'color-mix(in srgb, var(--success) 45%, var(--border))' : 'color-mix(in srgb, var(--danger) 50%, var(--border))'),
      color: ok ? 'var(--success)' : 'var(--danger)',
      background: ok ? 'color-mix(in srgb, var(--success) 10%, transparent)' : 'color-mix(in srgb, var(--danger) 10%, transparent)',
      borderRadius: 999,
      padding: '1px 7px',
      fontSize: 10,
      whiteSpace: 'nowrap',
    }}>{text}</span>
  )
}

function val<T>(probe: ProbeValue<T>): string {
  if (!probe.ok) return 'unavailable'
  if (probe.value === null || probe.value === undefined) return ''
  return String(probe.value)
}

function suffix(probe: ProbeValue<number>, unit: string): string {
  const v = val(probe)
  return v ? `${v}${unit}` : v
}

function bool(probe: ProbeValue<boolean>): string {
  if (!probe.ok) return 'unavailable'
  return probe.value ? 'enabled' : 'disabled'
}
