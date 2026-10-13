# Parity: ui-shell (`muxus-ui`, `muxus-app`)

`muxus-ui` is the MUI look of `client/src/theme.ts` in GPUI: theme tokens,
bundled fonts and icons, and the shared widgets. `muxus-app` is the main
window: its library holds the UI-free logic (host catalog model, sidebar
tree, tabs and pane layout, keymap) and the `muxus` binary holds the GPUI
views of the window chrome.

```bash
CARGO_TARGET_DIR=target cargo test -p muxus-app -p muxus-ui     # 291 tests
CARGO_TARGET_DIR=target cargo run -p muxus-app                   # the window
hack/shot.sh --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=emptytab out.png -- target/debug/muxus
```

`MUXUS_THEME=light|dark|os`, `MUXUS_WINDOW=fullscreen|maximized|WxH` and
`MUXUS_SCENE=empty|emptytab|overview|many` (the docs screenshots' demo
catalog and tabs; `many` adds 800 hosts) are development aids read at startup.

## Screenshot harness (`hack/`)

| Tool | Status | Note |
|---|---|---|
| `hack/shot.sh` + `shot_driver.py` | done | headless GNOME Shell in a capped systemd scope, private D-Bus, scratch HOME, monitor scale 2, `org.gnome.Shell.Screenshot`; `--dark` sets the desktop color scheme; `--step click:/rclick:/move:/wait:` drives the pointer through `org.gnome.Mutter.RemoteDesktop`; unmounts the document portal's FUSE mount on exit |
| `hack/compare.py` | done | side-by-side, amplified diff and blink GIF, share of differing pixels |
| `hack/build_fonts.py`, `hack/extract_icons.mjs` | done | rebuild `assets/fonts` and `assets/icons` from the client's packages |

The Electron build itself runs in the same harness (`electron --no-sandbox
--ozone-platform=wayland`, with `--home` pointing at a seeded demo home), so
muxus-rs can be compared with Electron pixel for pixel in one compositor.

## muxus-ui

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Palette light/dark, `sidebar`, `sidebarInk`, status text colors, `titleBarColors` | client/src/theme.ts | done | MUI `augmentColor` (light/dark/contrastText) and action colors computed as MUI does; unit tests against the client's values |
| Typography (13px base, `pxToRem` coefficient, variants, 450/550/650 weights) | client/src/theme.ts | done | variant table matches the client's computed styles |
| Fonts: Inter (static faces 400–800), JetBrains Mono 400/500/700 and 400/700 italic, Pure Nerd Font | client/src/main.tsx | done | GPUI rasterizes variable fonts at the default instance only, so weights 450/550/650 ship as static faces with exact `usWeightClass` |
| Icons: every `@mui/icons-material` icon the client imports, logo | client/src/** | done | 213 SVGs; logo rasterized synchronously at exact size (`icon::logo`) |
| Letter spacing + ellipsis (`label::Label`) | treeLabelSx, tabs, status bar, h6 | done | GPUI has no tracking; the element paints glyphs itself, Chrome's integer-baseline line model, ligatures off while tracked |
| IconButton (small/medium, colors, edge end, badge) | MUI IconButton | done | geometry measured: 28.57px small |
| Button (text/outlined/contained, small/medium, start icon) | MUI Button + MuiButton override | done | not yet compared against a client screenshot of a contained button |
| TextField (outlined small, start adornment, focus ring) | MUI OutlinedInput + override | done | wraps gpui-base's unstyled input; caret/selection/placeholder colors projected |
| Tooltip | MuiTooltip override | done | controls record their painted bounds and the bubble is centred on them 14px away with the 12×6 arrow, as MUI places it (`Tooltip::anchored`); `Tooltip::card` is the host hover card; compared with Electron's Settings tooltip |
| Menu / MenuItem / ListItemIcon / chord hints / divider | MuiMenu override | done | paper, border, shadow, 34.3px items, 36px icon column; `menu::popup` is modal (a backdrop closes it on any click outside, so hover cards stay hidden), anchored below its button or at the pointer, and kept 16px inside the window (`marginThreshold`); menus draw on `MENU_LAYER` above dialogs (a menu below its dialog was hidden yet took clicks) and a select's list or an autocomplete is non-modal (`Popup::non_modal`); compared with Electron's host, folder, panel, tab and appearance menus |
| Dialog (paper, title, content, actions, backdrop) | MuiDialog override | partial | not yet compared against a client dialog |
| Tabs / Tab | MuiTabs/MuiTab overrides | partial | not yet compared |
| Checkbox, Radio, Switch | MUI defaults | partial | MUI geometry, not yet compared |
| Select (closed field) | MUI Select | partial | the open list is a Menu the owner mounts |
| Chip, LinearProgress, Alert (standard/filled), Badge, Divider | MUI defaults + overrides | partial | Alert filled used by the toast host |
| Scrollbar | MuiCssBaseline scrollbar styles | partial | thumb overlay is display-only (no drag) |
| CircularProgress, Autocomplete, Slider, Table | MUI | missing | |

## muxus-app: logic (library, tested)

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Host catalog model | shared/src/api-types.ts, ws-protocol.ts | done | `muxus_app::model` re-exports the `muxus_core::api` types plus small helpers |
| Demo catalog | hack/demo-env.mjs, hack/capture.mjs | done | `model::demo` |
| Host grouping, display names, addresses, search text | host-organization.ts, managed-hosts.ts | done | |
| Saved hosts helpers | saved-hosts.ts | partial | display/address/search/filter/jump hops; export helpers not ported |
| Search scoring, best match | host-search.ts | done | |
| Folder tree, flattening, siblings, paths | host-tree.ts | done | arena-backed |
| Row clicks, tree window, navigation, selection, drag and drop rules | components/sidebar/{row-clicks,tree-window,tree-navigation,tree-selection,tree-dnd}.ts | done | |
| Folder mutations, host details, folder icons, kind icons, folder prefs, live counts, row geometry | components/sidebar/* | done | |
| `localeCompare` | ICU root collation | partial | `muxus_core::collate::locale_compare` (an approximation, documented there) |
| Pane layout | state/workspace-layout.ts | done | |
| Tabs store | state/tabs.ts | done | |
| Session actions (connect host/target, local terminal, launch guard, quick-connect test) | session-actions.ts | partial | tab side only; the session itself is the backend's |
| Keymap: chords, bindings, command catalog, hints | keymap/*.ts | done | `runKeyCommand` side effects live in the UI |
| Sidebar width | sidebar-width.ts | done | |

Ported test files: host-tree, host-tree-window, tree-dnd, tree-navigation,
tree-selection, row-clicks, host-search, managed-hosts, host-organization,
folder-mutations, tabs, workspace-layout, keymap, sidebar-width. Skipped
cases (they test modules outside this area): tabs.test.ts host SFTP actions
(4) and confirm dialogs (8); keymap.test.ts command-menu, send-secret and
focus-mode `run()` side effects.

## muxus-app: window chrome (binary)

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| App shell layout (top bar, command bar slot, sidebar left/right, canvas, status bar, focus mode) | layout/AppShell.tsx | done | |
| Top bar (menu, logo, title, workspace button, actions, badge, appearance) | layout/TopBar.tsx | partial | buttons without a ported target (multi-exec, command buttons, history, shortcuts, settings, SFTP) render and show tooltips only until the features are hooked up; appearance menu, focus mode and the terminal actions menu work |
| Window decorations | electron main window (titleBarOverlay) | done | `muxus_ui::widgets::window_frame`, shared by every window: drag, double-click maximize, resize edges, min/max/close drawn as Electron draws them on Linux, Chromium's shadow and rounded top corners on a floating window and no border line (measured in the same compositor; `hack/window_edge.py` guards the outline) |
| Sidebar panel + resize grip | layout/Sidebar.tsx, PanelResizeHandle.tsx | partial | pointer resize, double-click reset; keyboard resize and the docked file browser rail missing |
| Session sidebar (search, quick connect, Local terminal, empty states) | SessionSidebar.tsx | partial | local shell profiles, WSL, config error alert, selection bar missing |
| Host tree rows, folder rows, indent guides, live dots, hover card | sidebar/{HostTree,HostRow,FolderRow}.tsx | done | windowed (spacers + overscan) |
| Tree keyboard navigation and selection | useTreeKeyboard.ts, HostTree.tsx | partial | arrows, Home/End, Enter, Shift+Arrow, Ctrl+A, Escape; type-ahead and Alt+Arrow reorder missing |
| Tree drag and drop | useTreeDnd.ts | missing | the rules are ported (`tree_dnd`), the drag wiring is not |
| Sidebar context menus (host, folder, panel, local shell) | sidebar/*ContextMenu.tsx | partial | all items present with the client's enabled states; actions needing the host editor, folder dialog, launch dialog or store writes report "not available yet"; Connect, folder move/collapse, copy command and sidebar side work |
| Host sessions menu | sidebar/HostSessionsMenu.tsx | done | `ui/tab_actions.rs`; anchored at the host row's edge; status, details, the last two lines of output, New session |
| Tab strip (tabs, status dot, color flag, pin, close, numbers on Alt, new tab, all-tabs menu, split, zoom, close pane) | components/TabStrip.tsx | partial | overflow scroll arrows, rename dialog, cross-window transfer and the FLIP slide missing; drag reorder within and across panes works |
| Tab and pane context menus | components/TabStrip.tsx | partial | split/move/pin/duplicate/color/close work; rename, edit host, new window, reconnect, logging items missing or inert |
| Pane canvas, split dividers, zoom, active pane border, dimming | AppShell.tsx PaneCanvas | done | divider drag and double-click reset |
| Empty pane | components/EmptyPane.tsx | partial | Local terminal works; host picker popover and host editor wait for the integration pass |
| Session views | TerminalView, RemoteDesktopView, ManagementView | partial | terminals are live `muxus-term-view` views (see `app.md`); remote desktop and management tabs show a framed placeholder until their feature views are hooked up |
| Status bar | components/StatusBar.tsx | done | source messages, item menu and the polled host statistics (see `app.md`) |
| Action bar | components/ActionBar.tsx | partial | hidden, as it is without command buttons; the buttons themselves missing |
| Toasts | components/ToastHost.tsx | partial | filled alert, auto-hide, drawn above dialogs (`TOAST_LAYER`); Copy/Details actions missing |
| Global shortcuts | shortcuts.ts | partial | tab/pane commands, sidebar, focus mode; dialogs and terminal commands not wired; GPUI reports layout keys, not physical `code`s |
| Ctrl+Shift+I | electron hidden menu (toggleDevTools) | done | gpui-component binds it (Cmd+Alt+I on macOS) to its inspector in debug builds; `muxus_ui::init` unbinds it in the `Terminal` key context so a focused terminal always receives the chord. Electron hands it to xterm.js first too, but opens the developer tools when xterm leaves it unhandled (it encodes the chord only under the kitty keyboard protocol); release builds have no inspector |
| Layout preview | components/LayoutPreview.tsx | missing | |
| Preferences | state/prefs.ts | done | `muxus_store`'s `PrefsState`, read from and written to the backend (see `app.md`) |

## Measured differences

Reference shots: `docs/assets/screenshots/overview*.png` are from July and
predate the status bar and two demo hosts, so the comparison used fresh
captures of the current client (same `capture.mjs` recipe, 1420×880 at 2×)
and the Electron build run in the same headless compositor.

- **Overall, against Electron** (1600×1000 maximized, same compositor,
  demo catalog, no tabs): 0.43% (light) / 0.45% (dark) of device pixels
  differ by more than 24/255, the panel clock included; the rest is glyph
  edges.
- **Overall, against the browser capture** (1420×880 fullscreen): emptytab
  0.96% / 1.00%, empty 0.96% / 1.00%, overview 1.71% / 1.72% (the overview's
  terminal is a placeholder). Text offsets are tuned to Electron, which
  places text 1–3 device pixels apart from headless Chrome.
- **Text rasterization:** GPUI rasterizes with swash (no autohinting), Chrome
  with FreeType's light autohinter: cap heights come out about one device
  pixel shorter. Horizontal extents match to ±1 device pixel since tracked
  labels exist.
- **Baselines:** labels and paragraphs use Chrome's line model (ascent and
  descent rounded, integer baseline offset). Sidebar rows (+0.5px) and tab
  titles (-0.5px) carry a nudge; with it the sidebar, tab strip, top bar
  icons and empty pane match Electron to the device pixel. The workspace
  button label, the "Muxus" title and the search placeholder remain ±1
  device pixel off.
- **Top bar icons:** ±1 device pixel against the browser capture (SVG pixel
  snapping), exact against Electron.
- **Dividers:** rgba(0,0,0,0.08) over #f4f4f5 composites to 224 in GPUI and
  225 in Chrome (one level).
- **Fractional heights:** GPUI snaps every explicit size to device pixels
  before layout, Chrome accumulates fractions. A 34.29px menu item becomes
  34.5px, so a 13-item menu ends about 2–3px lower than in Electron.
- **Tooltip timing:** GPUI's hover delay and hide-on-leave apply; MUI's
  enter delay and the fade are not reproduced.
- **Window controls:** match Electron with GTK's default (Adwaita) theme; a
  desktop with another GTK theme changes Electron's controls but not ours.
  Every window (main, settings, SFTP) uses client decorations through
  `window_frame::app_window_options`; the close button goes through the
  window's close hooks (`on_close_request`).
- **Top bar tunnel badge:** follows the backend's forward events
  (`ForwardsSummary`: count, tooltip, amber while one is starting or
  interrupted) instead of a poll from the launcher.
- **Layers over dialogs:** a dialog backdrop occludes what is under it and
  re-sends the pointer position when it mounts (`hover::hover_refresh`), so
  tooltips and hover states under it end.

## Integration pass

The feature screens are wired into the shell (`app.md`): top bar buttons, sidebar menus, drag
and drop, the host sessions menu, the tab menu, tab overflow, the empty pane's cards, every
keyboard command.
