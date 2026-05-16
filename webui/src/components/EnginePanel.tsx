import { useQuery } from '@tanstack/react-query'
import { api, type EngineDiagnostics, type ProbeValue } from '../api/client'

export function EnginePanel() {
  const { data, isLoading, error } = useQuery({
    queryKey: ['engine'],
    queryFn: api.engine,
    staleTime: 2_000,
    refetchInterval: 5_000,
  })
  const { data: commands } = useQuery({
    queryKey: ['engine-commands'],
    queryFn: api.engineCommands,
    refetchInterval: 60000,
  })

  const driftProblems = data?.drift.filter(row => row.status !== 'match').length ?? 0

  return (
    <section style={{ padding: '16px 24px' }}>
      <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 12, marginBottom: 12 }}>
        <h2 style={{ fontSize: 13, margin: 0, color: '#cbd5e1' }}>Engine</h2>
        {data && (
          <div style={{ display: 'flex', gap: 8, alignItems: 'center' }}>
            <Badge ok={driftProblems === 0} text={driftProblems === 0 ? 'profile clean' : `${driftProblems} drift`} />
            <Badge ok={data.capabilities.every(c => c.available)} text={`${data.capabilities.filter(c => c.available).length}/${data.capabilities.length} capabilities`} />
          </div>
        )}
      </div>

      {isLoading && <div style={{ color: '#64748b', fontSize: 12 }}>Loading engine diagnostics...</div>}
      {error && <div style={{ color: '#ef4444', fontSize: 12 }}>Engine diagnostics unavailable</div>}
      {data && (
        <div style={{ display: 'grid', gridTemplateColumns: 'minmax(240px, 1fr) minmax(320px, 2fr)', gap: 16 }}>
          <Provenance data={data} />
          <Capabilities data={data} />
          <HttpStack data={data} />
          <DhtStack data={data} />
          <ProfileDrift data={data} />
          <CommandIndex commands={commands} />
        </div>
      )}
    </section>
  )
}

function Provenance({ data }: { data: EngineDiagnostics }) {
  const p = data.provenance
  return (
    <div>
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
    </div>
  )
}

function Capabilities({ data }: { data: EngineDiagnostics }) {
  return (
    <div>
      <Subhead>Capabilities</Subhead>
      <div style={{ display: 'grid', gridTemplateColumns: 'repeat(auto-fit, minmax(210px, 1fr))', gap: 8 }}>
        {data.capabilities.map(cap => (
          <div key={cap.key} style={{
            border: '1px solid #1e293b',
            borderRadius: 6,
            padding: '8px 10px',
            background: cap.available ? '#0f1a24' : '#1a1114',
            minWidth: 0,
          }}>
            <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 8 }}>
              <span style={{ fontSize: 12, color: '#e2e8f0', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{cap.label}</span>
              <Badge ok={cap.available} text={cap.available ? 'yes' : 'no'} />
            </div>
            <div title={cap.command} style={{ fontSize: 11, color: '#64748b', marginTop: 4, overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
              {cap.command}
            </div>
          </div>
        ))}
      </div>
    </div>
  )
}

function HttpStack({ data }: { data: EngineDiagnostics }) {
  const h = data.http
  return (
    <div style={{ gridColumn: '1 / -1' }}>
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
    </div>
  )
}

function DhtStack({ data }: { data: EngineDiagnostics }) {
  const d = data.dht
  return (
    <div style={{ gridColumn: '1 / -1' }}>
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
    </div>
  )
}

function ProfileDrift({ data }: { data: EngineDiagnostics }) {
  const rows = data.drift
  const problems = rows.filter(row => row.status !== 'match')
  return (
    <div style={{ gridColumn: '1 / -1' }}>
      <Subhead>Engine Profile Drift</Subhead>
      {problems.length === 0 && <div style={{ color: '#86efac', fontSize: 12 }}>Running profile matches rtorrentNG defaults</div>}
      {problems.length > 0 && (
        <div style={{ display: 'grid', gap: 6 }}>
          {problems.map(row => (
            <div key={row.key} style={{
              display: 'grid',
              gridTemplateColumns: '180px minmax(0, 1fr) minmax(0, 1fr)',
              gap: 10,
              alignItems: 'center',
              border: '1px solid #3f1d1d',
              borderRadius: 6,
              padding: '7px 9px',
              background: '#1a1114',
              fontSize: 12,
            }}>
              <div title={row.command} style={{ color: '#e2e8f0', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{row.label}</div>
              <div title={row.expected} style={{ color: '#64748b', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>Expected {row.expected}</div>
              <div title={row.actual ?? row.detail ?? ''} style={{ color: '#fca5a5', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>
                {row.status === 'unavailable' ? 'Unavailable' : `Actual ${row.actual ?? ''}`}
              </div>
            </div>
          ))}
        </div>
      )}
    </div>
  )
}

function CommandIndex({ commands }: { commands?: { ok: boolean; count: number; commands: string[]; error: string | null } }) {
  return (
    <div style={{ gridColumn: '1 / -1' }}>
      <Subhead>XMLRPC Command Surface</Subhead>
      {!commands && <div style={{ color: '#64748b', fontSize: 12 }}>Loading command index...</div>}
      {commands && !commands.ok && <div style={{ color: '#fca5a5', fontSize: 12 }}>Command index unavailable</div>}
      {commands?.ok && (
        <details>
          <summary style={{ color: '#cbd5e1', fontSize: 12, cursor: 'pointer' }}>{commands.count} commands exposed by rTorrent</summary>
          <div style={{
            marginTop: 8,
            maxHeight: 180,
            overflow: 'auto',
            display: 'grid',
            gridTemplateColumns: 'repeat(auto-fit, minmax(220px, 1fr))',
            gap: 4,
            fontFamily: 'ui-monospace, SFMono-Regular, Menlo, monospace',
            fontSize: 11,
            color: '#94a3b8',
          }}>
            {commands.commands.map(command => <div key={command} title={command} style={{ overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{command}</div>)}
          </div>
        </details>
      )}
    </div>
  )
}

function Rows({ rows }: { rows: [string, string][] }) {
  return (
    <div style={{ display: 'grid', gridTemplateColumns: '160px minmax(0, 1fr)', gap: '6px 12px', fontSize: 12 }}>
      {rows.map(([k, v]) => (
        <div key={k} style={{ display: 'contents' }}>
          <div style={{ color: '#64748b' }}>{k}</div>
          <div title={v} style={{ color: '#cbd5e1', overflow: 'hidden', textOverflow: 'ellipsis', whiteSpace: 'nowrap' }}>{v}</div>
        </div>
      ))}
    </div>
  )
}

function Subhead({ children }: { children: string }) {
  return <div style={{ fontSize: 11, textTransform: 'uppercase', color: '#64748b', marginBottom: 8 }}>{children}</div>
}

function Badge({ ok, text }: { ok: boolean; text: string }) {
  return (
    <span style={{
      border: '1px solid ' + (ok ? '#14532d' : '#7f1d1d'),
      color: ok ? '#86efac' : '#fca5a5',
      background: ok ? '#052e16' : '#450a0a',
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
