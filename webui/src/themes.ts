export type ThemeMode = 'dark' | 'light'

type ThemeTokens = Record<string, string>

interface ThemePalette {
  id: string
  label: string
  dark: ThemeTokens
  light: ThemeTokens
}

const darkBase = {
  bg: '#0d1117',
  panel: '#0f141d',
  surface: '#111827',
  surface2: '#1e2433',
  tableHead: '#1e2433',
  row: '#111720',
  rowAlt: '#18202c',
  selected: '#14203a',
  border: '#1e2433',
  borderStrong: '#334155',
  text: '#e2e8f0',
  muted: '#94a3b8',
  faint: '#64748b',
  shadow: 'rgba(0,0,0,0.45)',
}

const lightBase = {
  bg: '#f8fafc',
  panel: '#ffffff',
  surface: '#f1f5f9',
  surface2: '#e2e8f0',
  tableHead: '#e2e8f0',
  row: '#ffffff',
  rowAlt: '#eef3f8',
  selected: '#dbeafe',
  border: '#dbe3ee',
  borderStrong: '#94a3b8',
  text: '#172033',
  muted: '#475569',
  faint: '#64748b',
  shadow: 'rgba(15,23,42,0.18)',
}

function palette(
  id: string,
  label: string,
  accent: string,
  accentSoft: string,
  accentText: string,
  lightAccent: string,
  lightSoft: string,
): ThemePalette {
  return {
    id,
    label,
    dark: {
      ...darkBase,
      accent,
      accentSoft,
      accentText,
      success: '#22c55e',
      warning: '#f59e0b',
      danger: '#ef4444',
    },
    light: {
      ...lightBase,
      accent: lightAccent,
      accentSoft: lightSoft,
      accentText: '#0f172a',
      success: '#15803d',
      warning: '#b45309',
      danger: '#b91c1c',
    },
  }
}

export const PALETTES: ThemePalette[] = [
  palette('rtng', 'rtorrentNG', '#3b82f6', '#1e3a5f', '#bfdbfe', '#2563eb', '#dbeafe'),
  {
    id: 'sietch',
    label: 'Sietch Neon',
    dark: {
      bg: '#171512',
      panel: '#1f1b18',
      surface: '#28231f',
      surface2: '#332d27',
      tableHead: '#2c292d',
      row: '#211d1a',
      rowAlt: '#29231f',
      selected: '#3b2748',
      border: '#3c352f',
      borderStrong: '#5a4d43',
      text: '#ece4dc',
      muted: '#b9aaa5',
      faint: '#83736e',
      accent: '#b47cff',
      accentSoft: '#3b2748',
      accentText: '#eadcff',
      success: '#8cffd2',
      warning: '#d29a54',
      danger: '#ff6b78',
      shadow: 'rgba(0,0,0,0.55)',
    },
    light: {
      bg: '#f4eee6',
      panel: '#fff8ef',
      surface: '#eadfd3',
      surface2: '#ded1c6',
      tableHead: '#d8cfc9',
      row: '#fff8ef',
      rowAlt: '#efe3d8',
      selected: '#e8d7ff',
      border: '#d4c4b7',
      borderStrong: '#8d7c70',
      text: '#271f1e',
      muted: '#5f524d',
      faint: '#807169',
      accent: '#7d3cff',
      accentSoft: '#e8d7ff',
      accentText: '#251038',
      success: '#007a5b',
      warning: '#9a5b14',
      danger: '#b42336',
      shadow: 'rgba(50,35,25,0.2)',
    },
  },
  palette('nord', 'Nord', '#88c0d0', '#28475a', '#d8eef4', '#3b82a0', '#d9edf3'),
  palette('solarized', 'Solarized', '#268bd2', '#153d52', '#b9e1f7', '#268bd2', '#d6ecf7'),
  palette('gruvbox', 'Gruvbox', '#fabd2f', '#4b3512', '#fde68a', '#b45309', '#fef3c7'),
  palette('catppuccin', 'Catppuccin', '#cba6f7', '#3b2d58', '#eadcff', '#7c3aed', '#ede9fe'),
  palette('dracula', 'Dracula', '#bd93f9', '#3b2b63', '#eadcff', '#7c3aed', '#ede9fe'),
  palette('tokyo', 'Tokyo Night', '#7aa2f7', '#253966', '#dbeafe', '#3864d9', '#dbeafe'),
  palette('monokai', 'Monokai', '#a6e22e', '#2f4d19', '#e5ffc2', '#4d7c0f', '#e7f8cf'),
  palette('onedark', 'One Dark', '#61afef', '#203d55', '#d8ecff', '#2563eb', '#dbeafe'),
  palette('ayu', 'Ayu', '#ffb454', '#4a3217', '#ffe4b5', '#c2410c', '#ffedd5'),
  palette('everforest', 'Everforest', '#a7c080', '#32442f', '#e1efd0', '#4d7c0f', '#e7f5d8'),
  palette('rosepine', 'Rose Pine', '#ebbcba', '#55353f', '#ffe4e6', '#be123c', '#ffe4e6'),
  palette('kanagawa', 'Kanagawa', '#7e9cd8', '#2a3b5e', '#d9e5ff', '#3156a3', '#dbeafe'),
  palette('material', 'Material', '#80cbc4', '#214c49', '#d7fffb', '#0f766e', '#ccfbf1'),
  palette('github', 'GitHub', '#58a6ff', '#173a5e', '#dbeafe', '#0969da', '#ddf4ff'),
  palette('slate', 'Slate', '#38bdf8', '#164a63', '#e0f2fe', '#0284c7', '#e0f2fe'),
  palette('synthwave', 'Synthwave', '#ff7edb', '#5a2451', '#ffe0f7', '#c026d3', '#fae8ff'),
  palette('oceanic', 'Oceanic', '#5fb3b3', '#1f4b51', '#d4ffff', '#0e7490', '#cffafe'),
  palette('horizon', 'Horizon', '#f09383', '#5a302d', '#ffe3dd', '#dc2626', '#fee2e2'),
  palette('contrast', 'High Contrast', '#facc15', '#4d4300', '#fef9c3', '#ca8a04', '#fef9c3'),
]

export const THEME_STORAGE_KEY = 'rtng.theme'
export const THEME_MODE_STORAGE_KEY = 'rtng.themeMode'

export function findPalette(id: string): ThemePalette {
  return PALETTES.find(palette => palette.id === id) ?? PALETTES[0]
}

export function applyTheme(id: string, mode: ThemeMode) {
  const tokens = findPalette(id)[mode]
  const root = document.documentElement
  for (const [key, value] of Object.entries(tokens)) {
    root.style.setProperty(`--${kebab(key)}`, value)
  }
  root.style.colorScheme = mode
}

function kebab(value: string): string {
  return value
    .replace(/([a-z])([A-Z0-9])/g, '$1-$2')
    .replace(/[A-Z]/g, char => char.toLowerCase())
    .toLowerCase()
}
