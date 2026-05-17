export type MediaInferenceMode = 'full' | 'suffix' | 'hints' | 'off'

interface Props {
  mediaInference: MediaInferenceMode
  onMediaInference: (mode: MediaInferenceMode) => void
}

const MODES: Array<{ value: MediaInferenceMode; label: string; description: string }> = [
  { value: 'full', label: 'Full', description: 'Use suffixes plus name, category, tag, and path hints.' },
  { value: 'suffix', label: 'Suffix only', description: 'Classify only from visible file extensions.' },
  { value: 'hints', label: 'Hints only', description: 'Classify from names, categories, tags, and paths without suffix parsing.' },
  { value: 'off', label: 'Disabled', description: 'Show a neutral type icon for every torrent.' },
]

export function AppearancePanel({ mediaInference, onMediaInference }: Props) {
  return (
    <section style={{ padding: 18 }}>
      <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)', marginBottom: 4 }}>Appearance</div>
      <div style={{ fontSize: 12, color: 'var(--faint)', marginBottom: 14 }}>
        Configure presentation-only behavior for this browser.
      </div>

      <div className="rtng-card rtng-appearance-panel" style={panelStyle}>
        <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 10 }}>
          <div>
            <div style={{ fontSize: 12, color: 'var(--text)', fontWeight: 800 }}>Media type inference</div>
            <div style={{ fontSize: 11, color: 'var(--faint)', marginTop: 2 }}>Controls the icon and type filters only.</div>
          </div>
          <span style={{
            color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
            borderRadius: 999, padding: '2px 8px', fontSize: 11, fontWeight: 800,
          }}>{MODES.find(mode => mode.value === mediaInference)?.label}</span>
        </div>
        {MODES.map(mode => (
          <label key={mode.value} className="rtng-appearance-option" data-active={mediaInference === mode.value ? 'true' : 'false'} style={{
            display: 'grid', gridTemplateColumns: 'auto 1fr auto', gap: 10, alignItems: 'start',
            border: '1px solid ' + (mediaInference === mode.value ? 'var(--accent)' : 'var(--border)'),
            borderRadius: 6, padding: 10, background: mediaInference === mode.value ? 'var(--accent-soft)' : 'var(--surface)',
            cursor: 'pointer', marginTop: 8,
          }}>
            <input
              type="radio"
              name="mediaInference"
              checked={mediaInference === mode.value}
              onChange={() => onMediaInference(mode.value)}
              style={{ accentColor: 'var(--accent)', marginTop: 2 }}
            />
            <span>
              <span style={{ display: 'block', color: 'var(--text)', fontSize: 13, fontWeight: 600 }}>{mode.label}</span>
              <span style={{ display: 'block', color: 'var(--faint)', fontSize: 12, marginTop: 2 }}>{mode.description}</span>
            </span>
            {mediaInference === mode.value && (
              <span style={{
                color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
                borderRadius: 999, padding: '1px 7px', fontSize: 10, fontWeight: 700,
              }}>Active</span>
            )}
          </label>
        ))}
      </div>
    </section>
  )
}

const panelStyle: React.CSSProperties = {
  display: 'grid',
  gap: 0,
  maxWidth: 760,
  background: 'color-mix(in srgb, var(--surface) 84%, var(--bg))',
  border: '1px solid var(--border)',
  borderRadius: 8,
  padding: 12,
  boxShadow: 'inset 0 1px 0 rgba(255,255,255,0.03)',
}
