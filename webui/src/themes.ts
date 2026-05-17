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

function palette(id: string, label: string, dark: Partial<ThemeTokens>, light: Partial<ThemeTokens>): ThemePalette {
  return {
    id,
    label,
    dark: {
      ...darkBase,
      success: '#22c55e',
      warning: '#f59e0b',
      danger: '#ef4444',
      ...dark,
    },
    light: {
      ...lightBase,
      success: '#15803d',
      warning: '#b45309',
      danger: '#b91c1c',
      ...light,
    },
  }
}

export const PALETTES: ThemePalette[] = [
  palette('tng', 'TorrentNG', {
    bg: '#0b1220', panel: '#111827', surface: '#162033', surface2: '#202b42',
    tableHead: '#1b2740', row: '#101826', rowAlt: '#152033', selected: '#17335f',
    border: '#22324d', borderStrong: '#40516f', text: '#e6edf7', muted: '#a9b8cc', faint: '#73849c',
    accent: '#4f8cff', accentSoft: '#17335f', accentText: '#dbe8ff', shadow: 'rgba(2,6,23,0.52)',
  }, {
    bg: '#eef5ff', panel: '#fdfefe', surface: '#e3edf9', surface2: '#cfdef0',
    tableHead: '#d7e5f6', row: '#ffffff', rowAlt: '#edf4fc', selected: '#cfe2ff',
    border: '#bfd0e5', borderStrong: '#6f86a5', text: '#142235', muted: '#405168', faint: '#687991',
    accent: '#1f62d1', accentSoft: '#cfe2ff', accentText: '#0b1f44', shadow: 'rgba(25,55,96,0.22)',
  }),
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
  palette('nord', 'Nord', {
    bg: '#242b36', panel: '#2e3644', surface: '#363f50', surface2: '#414c60',
    tableHead: '#3b4558', row: '#2b3340', rowAlt: '#323b4b', selected: '#344d5d',
    border: '#465267', borderStrong: '#68758d', text: '#eceff4', muted: '#c3cad6', faint: '#8f9aaa',
    accent: '#88c0d0', accentSoft: '#344d5d', accentText: '#e5f7fb', shadow: 'rgba(9,12,18,0.45)',
  }, {
    bg: '#e8edf3', panel: '#f7f9fc', surface: '#dce4ee', surface2: '#ccd7e5',
    tableHead: '#d2dce8', row: '#fbfcfe', rowAlt: '#edf2f7', selected: '#cce5ec',
    border: '#bac7d7', borderStrong: '#74849a', text: '#243040', muted: '#4e5f74', faint: '#758397',
    accent: '#397f91', accentSoft: '#cce5ec', accentText: '#14333d', shadow: 'rgba(35,48,64,0.22)',
  }),
  palette('solarized', 'Solarized', {
    bg: '#002b36', panel: '#073642', surface: '#0b4654', surface2: '#155565',
    tableHead: '#124d5c', row: '#05313d', rowAlt: '#0a3c49', selected: '#123f5d',
    border: '#1d5b69', borderStrong: '#4d7d87', text: '#eee8d5', muted: '#b8ad8b', faint: '#7f8c8d',
    accent: '#268bd2', accentSoft: '#123f5d', accentText: '#d6ecf7', shadow: 'rgba(0,18,23,0.52)',
  }, {
    bg: '#fdf6e3', panel: '#fffaf0', surface: '#eee8d5', surface2: '#e0d7bd',
    tableHead: '#e7dec8', row: '#fffaf0', rowAlt: '#f4edda', selected: '#d7ecf8',
    border: '#d5c9aa', borderStrong: '#8b7d5b', text: '#263238', muted: '#586e75', faint: '#839496',
    accent: '#268bd2', accentSoft: '#d7ecf8', accentText: '#073642', shadow: 'rgba(82,65,27,0.2)',
  }),
  palette('gruvbox', 'Gruvbox', {
    bg: '#1d2021', panel: '#282828', surface: '#32302f', surface2: '#3c3836',
    tableHead: '#3a332c', row: '#242321', rowAlt: '#2d2a26', selected: '#4b3512',
    border: '#504945', borderStrong: '#7c6f64', text: '#ebdbb2', muted: '#bdae93', faint: '#928374',
    accent: '#fabd2f', accentSoft: '#4b3512', accentText: '#fff2b3', shadow: 'rgba(0,0,0,0.48)',
  }, {
    bg: '#f2e5bc', panel: '#fbf1c7', surface: '#ebdbb2', surface2: '#d5c4a1',
    tableHead: '#e4d3ab', row: '#fff7d5', rowAlt: '#f2e5bc', selected: '#f8dfa2',
    border: '#c9b483', borderStrong: '#8c7a52', text: '#3c3836', muted: '#665c54', faint: '#7c6f64',
    accent: '#af3a03', accentSoft: '#f8dfa2', accentText: '#321600', shadow: 'rgba(60,48,24,0.22)',
  }),
  palette('catppuccin', 'Catppuccin', {
    bg: '#1e1e2e', panel: '#242438', surface: '#2d2d46', surface2: '#383857',
    tableHead: '#33334f', row: '#222235', rowAlt: '#292941', selected: '#3b2d58',
    border: '#454563', borderStrong: '#6c6f85', text: '#cdd6f4', muted: '#a6adc8', faint: '#7f849c',
    accent: '#cba6f7', accentSoft: '#3b2d58', accentText: '#f1e4ff', shadow: 'rgba(8,8,18,0.5)',
  }, {
    bg: '#eff1f5', panel: '#ffffff', surface: '#e6e9ef', surface2: '#ccd0da',
    tableHead: '#dce0e8', row: '#ffffff', rowAlt: '#f3f5f9', selected: '#eadcff',
    border: '#bcc0cc', borderStrong: '#7c7f93', text: '#1e1e2e', muted: '#4c4f69', faint: '#6c6f85',
    accent: '#8839ef', accentSoft: '#eadcff', accentText: '#291044', shadow: 'rgba(49,50,68,0.2)',
  }),
  palette('dracula', 'Dracula', {
    bg: '#282a36', panel: '#303241', surface: '#3a3c4d', surface2: '#44475a',
    tableHead: '#3f4154', row: '#2d2f3d', rowAlt: '#353747', selected: '#4c356b',
    border: '#4f5268', borderStrong: '#777a94', text: '#f8f8f2', muted: '#c8c8c2', faint: '#8f90a0',
    accent: '#bd93f9', accentSoft: '#4c356b', accentText: '#f0ddff', success: '#50fa7b', warning: '#ffb86c', danger: '#ff5555',
    shadow: 'rgba(14,14,22,0.48)',
  }, {
    bg: '#f5f2fb', panel: '#fffaff', surface: '#e9e1f5', surface2: '#d8cbe8',
    tableHead: '#e2d8ef', row: '#fffaff', rowAlt: '#f3edf9', selected: '#e4d2ff',
    border: '#cbbbdf', borderStrong: '#806e9b', text: '#2b2435', muted: '#5f536c', faint: '#81758d',
    accent: '#7c3aed', accentSoft: '#e4d2ff', accentText: '#291044', shadow: 'rgba(56,38,83,0.22)',
  }),
  palette('tokyo', 'Tokyo Night', {
    bg: '#16161e', panel: '#1a1b26', surface: '#24283b', surface2: '#2f354d',
    tableHead: '#292e42', row: '#1b1d2b', rowAlt: '#22263a', selected: '#253966',
    border: '#3b4261', borderStrong: '#565f89', text: '#c0caf5', muted: '#9aa5ce', faint: '#737aa2',
    accent: '#7aa2f7', accentSoft: '#253966', accentText: '#dbeafe', shadow: 'rgba(3,4,10,0.52)',
  }, {
    bg: '#e9edf7', panel: '#fbfcff', surface: '#dfe5f2', surface2: '#cbd4e6',
    tableHead: '#d5ddec', row: '#ffffff', rowAlt: '#eef2f9', selected: '#d7e3ff',
    border: '#bac5d8', borderStrong: '#697896', text: '#1f2335', muted: '#4d5975', faint: '#747f9a',
    accent: '#345cc8', accentSoft: '#d7e3ff', accentText: '#111b3b', shadow: 'rgba(31,35,53,0.22)',
  }),
  palette('monokai', 'Monokai', {
    bg: '#1f201b', panel: '#272822', surface: '#313328', surface2: '#3e412f',
    tableHead: '#36382b', row: '#25261f', rowAlt: '#2c2e24', selected: '#33451f',
    border: '#4a4d38', borderStrong: '#70745a', text: '#f8f8f2', muted: '#c8c8bd', faint: '#8b8d7d',
    accent: '#a6e22e', accentSoft: '#33451f', accentText: '#eaffbd', success: '#a6e22e', warning: '#fd971f', danger: '#f92672',
    shadow: 'rgba(7,8,6,0.5)',
  }, {
    bg: '#f5f2dc', panel: '#fffdee', surface: '#ebe7c9', surface2: '#d8d3ac',
    tableHead: '#e3dec0', row: '#fffdee', rowAlt: '#f3efd7', selected: '#dff4b8',
    border: '#c9c192', borderStrong: '#807846', text: '#272822', muted: '#555845', faint: '#78775e',
    accent: '#4d7c0f', accentSoft: '#dff4b8', accentText: '#172407', shadow: 'rgba(47,48,23,0.22)',
  }),
  palette('onedark', 'One Dark', {
    bg: '#21252b', panel: '#282c34', surface: '#303641', surface2: '#3a4350',
    tableHead: '#343b47', row: '#252a32', rowAlt: '#2c323c', selected: '#203d55',
    border: '#444b58', borderStrong: '#6b7280', text: '#abb2bf', muted: '#8f98a8', faint: '#6b7280',
    accent: '#61afef', accentSoft: '#203d55', accentText: '#d8ecff', success: '#98c379', warning: '#e5c07b', danger: '#e06c75',
    shadow: 'rgba(7,9,13,0.48)',
  }, {
    bg: '#edf0f5', panel: '#fbfcfe', surface: '#e1e5ed', surface2: '#cdd3dd',
    tableHead: '#d7dce5', row: '#ffffff', rowAlt: '#f0f3f7', selected: '#d3e9ff',
    border: '#b9c1ce', borderStrong: '#717b8d', text: '#242936', muted: '#4b5565', faint: '#747b89',
    accent: '#2563a8', accentSoft: '#d3e9ff', accentText: '#0d2238', shadow: 'rgba(36,41,54,0.21)',
  }),
  palette('ayu', 'Ayu', {
    bg: '#0f1419', panel: '#151a1f', surface: '#1f2429', surface2: '#2a2f34',
    tableHead: '#272b30', row: '#14191e', rowAlt: '#1b2025', selected: '#4a3217',
    border: '#343a40', borderStrong: '#5c6773', text: '#e6e1cf', muted: '#b8ad9f', faint: '#7f8a96',
    accent: '#ffb454', accentSoft: '#4a3217', accentText: '#ffe4b5', shadow: 'rgba(0,0,0,0.5)',
  }, {
    bg: '#fbf0dc', panel: '#fff9ef', surface: '#efe1c8', surface2: '#dec9a9',
    tableHead: '#e8d7bc', row: '#fff9ef', rowAlt: '#f6ead4', selected: '#ffe0b0',
    border: '#ceb991', borderStrong: '#8d7043', text: '#2b251d', muted: '#625241', faint: '#82705a',
    accent: '#c2410c', accentSoft: '#ffe0b0', accentText: '#3b1604', shadow: 'rgba(66,43,13,0.22)',
  }),
  palette('everforest', 'Everforest', {
    bg: '#232a2e', panel: '#2b3339', surface: '#343f44', surface2: '#3d484d',
    tableHead: '#3a454a', row: '#293137', rowAlt: '#303a40', selected: '#32442f',
    border: '#4f5b58', borderStrong: '#7a8478', text: '#d3c6aa', muted: '#a7b08a', faint: '#859289',
    accent: '#a7c080', accentSoft: '#32442f', accentText: '#e1efd0', success: '#a7c080', warning: '#dbbc7f', danger: '#e67e80',
    shadow: 'rgba(8,12,10,0.48)',
  }, {
    bg: '#f0eed9', panel: '#fffbea', surface: '#e6e1c5', surface2: '#d4ceb0',
    tableHead: '#ded8ba', row: '#fffbea', rowAlt: '#f4f0dc', selected: '#dcebc9',
    border: '#c4bd99', borderStrong: '#7b775d', text: '#2f383e', muted: '#58635a', faint: '#7a8478',
    accent: '#4d7c0f', accentSoft: '#dcebc9', accentText: '#18270a', shadow: 'rgba(47,56,62,0.2)',
  }),
  palette('rosepine', 'Rose Pine', {
    bg: '#191724', panel: '#1f1d2e', surface: '#26233a', surface2: '#312d4a',
    tableHead: '#2c2942', row: '#1d1a2a', rowAlt: '#242136', selected: '#55353f',
    border: '#403d52', borderStrong: '#6e6a86', text: '#e0def4', muted: '#b5adc9', faint: '#908caa',
    accent: '#ebbcba', accentSoft: '#55353f', accentText: '#ffe4e6', success: '#9ccfd8', warning: '#f6c177', danger: '#eb6f92',
    shadow: 'rgba(8,7,13,0.52)',
  }, {
    bg: '#faf4ed', panel: '#fffaf6', surface: '#f2e9e1', surface2: '#dfdad9',
    tableHead: '#ebe3dd', row: '#fffaf6', rowAlt: '#f7efe8', selected: '#ffdfe5',
    border: '#d8cac0', borderStrong: '#8b7d76', text: '#372d36', muted: '#6e5967', faint: '#8f7b86',
    accent: '#b4637a', accentSoft: '#ffdfe5', accentText: '#3f1723', shadow: 'rgba(55,45,54,0.2)',
  }),
  palette('kanagawa', 'Kanagawa', {
    bg: '#1f1f28', panel: '#252535', surface: '#2a2a3a', surface2: '#363646',
    tableHead: '#303044', row: '#232331', rowAlt: '#2a2a38', selected: '#2a3b5e',
    border: '#49443c', borderStrong: '#727169', text: '#dcd7ba', muted: '#a6a69c', faint: '#7e7e76',
    accent: '#7e9cd8', accentSoft: '#2a3b5e', accentText: '#d9e5ff', success: '#98bb6c', warning: '#e6c384', danger: '#e46876',
    shadow: 'rgba(7,7,11,0.5)',
  }, {
    bg: '#f2ecdc', panel: '#fff9ec', surface: '#e7dcc8', surface2: '#d4c6ad',
    tableHead: '#ded2bd', row: '#fff9ec', rowAlt: '#f5eedf', selected: '#dbe6ff',
    border: '#c7b99e', borderStrong: '#80735d', text: '#2a2a37', muted: '#5f5a4e', faint: '#7d776b',
    accent: '#3156a3', accentSoft: '#dbe6ff', accentText: '#101e3e', shadow: 'rgba(50,43,29,0.22)',
  }),
  palette('material', 'Material', {
    bg: '#102027', panel: '#162b32', surface: '#1d3840', surface2: '#294850',
    tableHead: '#243f47', row: '#142930', rowAlt: '#1a333b', selected: '#214c49',
    border: '#34565e', borderStrong: '#6f8f94', text: '#eeffff', muted: '#b2ccd6', faint: '#77929a',
    accent: '#80cbc4', accentSoft: '#214c49', accentText: '#d7fffb', success: '#c3e88d', warning: '#ffcb6b', danger: '#f07178',
    shadow: 'rgba(0,12,16,0.5)',
  }, {
    bg: '#e9f6f5', panel: '#fbfffe', surface: '#d7eeeb', surface2: '#bee0dc',
    tableHead: '#cde8e4', row: '#fbfffe', rowAlt: '#eef9f7', selected: '#c7f3ee',
    border: '#acd0cb', borderStrong: '#5f8984', text: '#173033', muted: '#49656a', faint: '#6f888b',
    accent: '#00796b', accentSoft: '#c7f3ee', accentText: '#062b26', shadow: 'rgba(23,48,51,0.2)',
  }),
  palette('github', 'GitHub', {
    bg: '#0d1117', panel: '#161b22', surface: '#21262d', surface2: '#30363d',
    tableHead: '#242c35', row: '#111820', rowAlt: '#17202a', selected: '#173a5e',
    border: '#30363d', borderStrong: '#57606a', text: '#e6edf3', muted: '#8b949e', faint: '#6e7681',
    accent: '#58a6ff', accentSoft: '#173a5e', accentText: '#dbeafe', success: '#3fb950', warning: '#d29922', danger: '#f85149',
    shadow: 'rgba(1,4,9,0.55)',
  }, {
    bg: '#f6f8fa', panel: '#ffffff', surface: '#eef2f6', surface2: '#d8dee4',
    tableHead: '#eaeef2', row: '#ffffff', rowAlt: '#f6f8fa', selected: '#ddf4ff',
    border: '#d0d7de', borderStrong: '#8c959f', text: '#1f2328', muted: '#57606a', faint: '#6e7781',
    accent: '#0969da', accentSoft: '#ddf4ff', accentText: '#0a3069', shadow: 'rgba(31,35,40,0.18)',
  }),
  palette('slate', 'Slate', {
    bg: '#15191f', panel: '#1d232b', surface: '#252d37', surface2: '#303947',
    tableHead: '#2c3541', row: '#1a2028', rowAlt: '#202832', selected: '#164a63',
    border: '#3a4655', borderStrong: '#697585', text: '#e5e7eb', muted: '#a7b0bd', faint: '#7b8493',
    accent: '#38bdf8', accentSoft: '#164a63', accentText: '#e0f2fe', shadow: 'rgba(5,8,12,0.48)',
  }, {
    bg: '#edf2f7', panel: '#fbfdff', surface: '#e2e8f0', surface2: '#cbd5e1',
    tableHead: '#d8e0ea', row: '#ffffff', rowAlt: '#f1f5f9', selected: '#d8f0ff',
    border: '#c0cad6', borderStrong: '#718096', text: '#1e293b', muted: '#475569', faint: '#64748b',
    accent: '#0284c7', accentSoft: '#d8f0ff', accentText: '#082f49', shadow: 'rgba(30,41,59,0.2)',
  }),
  palette('synthwave', 'Synthwave', {
    bg: '#241033', panel: '#2d1644', surface: '#3b1e59', surface2: '#4c276f',
    tableHead: '#432363', row: '#2a143e', rowAlt: '#341a4d', selected: '#5a2451',
    border: '#673b80', borderStrong: '#965bb0', text: '#fff1ff', muted: '#f1b3e9', faint: '#bd83c7',
    accent: '#ff7edb', accentSoft: '#5a2451', accentText: '#ffe0f7', success: '#72f1b8', warning: '#fede5d', danger: '#fe4450',
    shadow: 'rgba(15,2,24,0.55)',
  }, {
    bg: '#fff0fb', panel: '#fffaff', surface: '#f5dcff', surface2: '#eac1fb',
    tableHead: '#f0d1ff', row: '#fffaff', rowAlt: '#fff1fb', selected: '#f5d4ff',
    border: '#dfa9ef', borderStrong: '#9a5daf', text: '#371643', muted: '#6d3d7d', faint: '#905e9e',
    accent: '#c026d3', accentSoft: '#f5d4ff', accentText: '#3b0d45', shadow: 'rgba(74,23,91,0.22)',
  }),
  palette('oceanic', 'Oceanic', {
    bg: '#102a2f', panel: '#17363c', surface: '#20444b', surface2: '#2a535a',
    tableHead: '#264c53', row: '#142f35', rowAlt: '#1a3a41', selected: '#1f4b51',
    border: '#376168', borderStrong: '#6a9296', text: '#d8ffff', muted: '#a8cfcf', faint: '#7a9da0',
    accent: '#5fb3b3', accentSoft: '#1f4b51', accentText: '#d4ffff', shadow: 'rgba(0,13,16,0.5)',
  }, {
    bg: '#e8f7f7', panel: '#fbffff', surface: '#d5eeee', surface2: '#bee0e0',
    tableHead: '#cbe8e8', row: '#fbffff', rowAlt: '#effafa', selected: '#cffafe',
    border: '#accfcf', borderStrong: '#638a8e', text: '#183338', muted: '#4c686d', faint: '#728d90',
    accent: '#0e7490', accentSoft: '#cffafe', accentText: '#07323d', shadow: 'rgba(24,51,56,0.2)',
  }),
  palette('horizon', 'Horizon', {
    bg: '#201417', panel: '#2a1b1f', surface: '#352328', surface2: '#432d32',
    tableHead: '#3d282e', row: '#25181c', rowAlt: '#2f1f24', selected: '#5a302d',
    border: '#51383d', borderStrong: '#7f5e62', text: '#fdf0ed', muted: '#d6aaa3', faint: '#a17976',
    accent: '#f09383', accentSoft: '#5a302d', accentText: '#ffe3dd', success: '#26bbd9', warning: '#f9cb40', danger: '#e95678',
    shadow: 'rgba(12,4,6,0.5)',
  }, {
    bg: '#fff1ee', panel: '#fffafa', surface: '#f6ded8', surface2: '#e9c6be',
    tableHead: '#f0d4cd', row: '#fffafa', rowAlt: '#fff3f0', selected: '#ffd9d2',
    border: '#dcb5ad', borderStrong: '#946c65', text: '#3a2022', muted: '#704b4d', faint: '#936e6d',
    accent: '#dc2626', accentSoft: '#ffd9d2', accentText: '#450a0a', shadow: 'rgba(74,32,28,0.22)',
  }),
  palette('contrast', 'High Contrast', {
    bg: '#050505', panel: '#0d0d0d', surface: '#171717', surface2: '#242424',
    tableHead: '#202020', row: '#0a0a0a', rowAlt: '#141414', selected: '#4d4300',
    border: '#3a3a3a', borderStrong: '#8a8a8a', text: '#ffffff', muted: '#d4d4d4', faint: '#a3a3a3',
    accent: '#facc15', accentSoft: '#4d4300', accentText: '#fff8b0', success: '#22c55e', warning: '#facc15', danger: '#ff4d4d',
    shadow: 'rgba(0,0,0,0.75)',
  }, {
    bg: '#f4f4f0', panel: '#ffffff', surface: '#e6e6df', surface2: '#d2d2c8',
    tableHead: '#deded6', row: '#ffffff', rowAlt: '#f0f0ea', selected: '#fff2a8',
    border: '#b8b8aa', borderStrong: '#595950', text: '#0a0a0a', muted: '#333333', faint: '#5f5f5a',
    accent: '#9a6a00', accentSoft: '#fff2a8', accentText: '#211600', shadow: 'rgba(0,0,0,0.26)',
  }),
]

export const THEME_STORAGE_KEY = 'tng.theme'
export const THEME_MODE_STORAGE_KEY = 'tng.themeMode'

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
