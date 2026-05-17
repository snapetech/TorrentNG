import type { ThemeMode } from '../themes'

export type MediaInferenceMode = 'full' | 'suffix' | 'hints' | 'off'

interface Props {
  mediaInference: MediaInferenceMode
  onMediaInference: (mode: MediaInferenceMode) => void
  themes?: Array<{ id: string; label: string; dark: Record<string, string>; light: Record<string, string> }>
  themeId?: string
  themeMode?: ThemeMode
  onTheme?: (id: string) => void
  onThemeMode?: (mode: ThemeMode) => void
}

const MODES: Array<{ value: MediaInferenceMode; label: string; description: string }> = [
  { value: 'full', label: 'Full', description: 'Use suffixes plus name, category, tag, and path hints.' },
  { value: 'suffix', label: 'Suffix only', description: 'Classify only from visible file extensions.' },
  { value: 'hints', label: 'Hints only', description: 'Classify from names, categories, tags, and paths without suffix parsing.' },
  { value: 'off', label: 'Disabled', description: 'Show a neutral type icon for every torrent.' },
]

export function AppearancePanel({ mediaInference, onMediaInference, themes = [], themeId, themeMode = 'dark', onTheme, onThemeMode }: Props) {
  const alternateMode: ThemeMode = themeMode === 'dark' ? 'light' : 'dark'

  return (
    <section style={{ padding: 18 }}>
      <div style={{ fontSize: 14, fontWeight: 700, color: 'var(--text)', marginBottom: 4 }}>Appearance</div>
      <div style={{ fontSize: 12, color: 'var(--faint)', marginBottom: 14 }}>
        Configure presentation-only behavior for this browser.
      </div>

      {themes.length > 0 && (
        <div className="rtng-card rtng-appearance-panel" style={{ ...panelStyle, marginBottom: 12 }}>
          <div style={{ display: 'flex', alignItems: 'center', justifyContent: 'space-between', gap: 10, marginBottom: 10 }}>
            <div>
              <div style={{ fontSize: 12, color: 'var(--text)', fontWeight: 800 }}>Theme palette</div>
              <div style={{ fontSize: 11, color: 'var(--faint)', marginTop: 2 }}>Preview the full shell colors used by each theme.</div>
            </div>
            <button
              type="button"
              onClick={() => onThemeMode?.(alternateMode)}
              style={{
                color: 'var(--accent-text)', background: 'var(--accent-soft)', border: '1px solid var(--accent)',
                borderRadius: 999, padding: '3px 9px', fontSize: 11, fontWeight: 800, cursor: 'pointer',
              }}
            >
              {themeMode === 'dark' ? 'Dark' : 'Light'}
            </button>
          </div>

          <div className="rtng-theme-gallery">
            {themes.map(theme => {
              const tokens = theme[themeMode]
              const active = theme.id === themeId
              return (
                <button
                  key={theme.id}
                  type="button"
                  className="rtng-theme-card"
                  data-active={active ? 'true' : 'false'}
                  onClick={() => onTheme?.(theme.id)}
                  style={{
                    ['--preview-bg' as string]: tokens.bg,
                    ['--preview-panel' as string]: tokens.panel,
                    ['--preview-surface' as string]: tokens.surface,
                    ['--preview-row' as string]: tokens.row,
                    ['--preview-alt' as string]: tokens.rowAlt,
                    ['--preview-accent' as string]: tokens.accent,
                    ['--preview-border' as string]: tokens.borderStrong,
                    ['--preview-text' as string]: tokens.text,
                    ['--preview-muted' as string]: tokens.muted,
                    ['--preview-shadow' as string]: tokens.shadow,
                  }}
                >
                  <span className="rtng-theme-card-preview">
                    <span className="rtng-theme-card-bar" />
                    <span className="rtng-theme-card-row" />
                    <span className="rtng-theme-card-row" />
                    <span className="rtng-theme-card-row" />
                  </span>
                  <span className="rtng-theme-card-label">
                    <span>{theme.label}</span>
                    {active && <span>Active</span>}
                  </span>
                </button>
              )
            })}
          </div>
        </div>
      )}

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
