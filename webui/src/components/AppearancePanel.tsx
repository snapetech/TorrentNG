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
      <div style={{ fontSize: 14, fontWeight: 700, color: '#e2e8f0', marginBottom: 4 }}>Appearance</div>
      <div style={{ fontSize: 12, color: '#64748b', marginBottom: 14 }}>
        Configure presentation-only behavior for this browser.
      </div>

      <div style={{ display: 'grid', gap: 8, maxWidth: 620 }}>
        <div style={{ fontSize: 11, color: '#64748b', fontWeight: 700, textTransform: 'uppercase' }}>
          Media type inference
        </div>
        {MODES.map(mode => (
          <label key={mode.value} style={{
            display: 'grid', gridTemplateColumns: 'auto 1fr', gap: 10, alignItems: 'start',
            border: '1px solid ' + (mediaInference === mode.value ? '#3b82f6' : '#1e2433'),
            borderRadius: 6, padding: 10, background: mediaInference === mode.value ? '#10203a' : '#0d1117',
            cursor: 'pointer',
          }}>
            <input
              type="radio"
              name="mediaInference"
              checked={mediaInference === mode.value}
              onChange={() => onMediaInference(mode.value)}
              style={{ accentColor: '#3b82f6', marginTop: 2 }}
            />
            <span>
              <span style={{ display: 'block', color: '#cbd5e1', fontSize: 13, fontWeight: 600 }}>{mode.label}</span>
              <span style={{ display: 'block', color: '#64748b', fontSize: 12, marginTop: 2 }}>{mode.description}</span>
            </span>
          </label>
        ))}
      </div>
    </section>
  )
}
