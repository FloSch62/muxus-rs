const nordCopy = {
  id: 'scheme-nord-copy', name: 'Nord copy',
  colors: { background: '#2e3440', foreground: '#d8dee9', cursor: '#d8dee9', selectionBackground: '#434c5e',
    black: '#3b4252', red: '#bf616a', green: '#a3be8c', yellow: '#ebcb8b', blue: '#81a1c1', magenta: '#b48ead', cyan: '#88c0d0', white: '#e5e9f0',
    brightBlack: '#4c566a', brightRed: '#bf616a', brightGreen: '#a3be8c', brightYellow: '#ebcb8b', brightBlue: '#81a1c1', brightMagenta: '#b48ead', brightCyan: '#8fbcbb', brightWhite: '#eceff4' },
};
const custom = { id: 'nokia-sros', name: 'Nokia SR OS custom', rules: [{ id: 'alarm', keyword: 'MAJOR', foreground: '#ffffff', background: '#b91c1c', caseSensitive: true, wholeWord: true }] };
export const cases = [
  { name: 'legacy-v0', stored: { version: 0, state: { terminalScheme: 'muxus', termName: 'xterm-kitty', monoFontSize: 16, themeMode: 'sepia', lineHeight: 1.2, futureKey: { nested: [1, 2] }, sidebarWidth: 248 } } },
  { name: 'legacy-v8', stored: { version: 8, state: {
      futureFirst: true,
      terminalScheme: 'nord', lightTerminalScheme: 'paper', themeMode: 'dark', sshKeepaliveIntervalSeconds: -1,
      pasteLineDelayMs: 250, pasteCharDelayMs: 1.5, statusBarItems: ['cpu', 'cpu'], showStatusBar: true,
      inactivePaneDimStrength: 0.4, dimInactivePanes: 'yes', customTerminalSchemes: [nordCopy],
      sidebarCollapsedFolders: ['folder:prod'], sidebarFolderStyles: { 'folder:prod': { color: '#ef5350', icon: 'cloud' }, bad: { colour: 'x' } },
      sidebarFolderOrder: { root: ['folder:prod', 'folder:lab'] }, sidebarEmptyFolders: [1],
      localShellProfiles: [{ id: 'ubuntu', name: 'Ubuntu', shell: 'wsl.exe', args: ['-d', 'Ubuntu'], cwd: 'C:\\work', startupCommand: 'cd project' }],
      defaultLocalShellProfileId: 'gone', keybindings: { 'tab.next': ['Ctrl+Tab'], 'tab.prev': [] },
      keywordHighlightProfiles: [custom],
      commandButtons: [{ id: 'uptime', label: 'Uptime', command: 'uptime', sendEnter: true, color: 'neon', groupId: 'juniper' }, { id: 'broken' }],
      x11Enabled: true, interfaceZoom: 1.25, sidebarWidth: 248, scrollback: 50000,
    } } },
  { name: 'v15-seeded-builtins', stored: { version: 15, state: { keywordHighlightProfiles: 'BUILTIN_UNNAMED', commandButtonGroups: [{ id: 'juniper', name: 'Juniper' }, { id: 'juniper', name: 'Twice' }], selectedCommandButtonGroup: 'juniper', commandButtons: [{ id: 'j', label: 'BGP', command: 'show bgp', sendEnter: false, groupId: 'juniper', color: 'blue', secretId: 's1' }] } } },
  { name: 'current-v17-unvalidated', touch: true, stored: { version: 17, state: { themeMode: 'sepia', monoFontSize: '16', fontFamily: 'Iosevka', extra: 1, keybindings: { a: ['b'] } } } },
  { name: 'newer-v18', stored: { version: 18, state: { themeMode: 'light', newPref: 'x', sshSessionSummary: 'yes', sidebarPosition: 'right' } } },
  { name: 'versionless', touch: true, stored: { state: { themeMode: 'dark', monoFontSize: 12 } } },
  { name: 'not-json', touch: true, raw: '{not json' },
  { name: 'empty', touch: true, raw: null },
];
