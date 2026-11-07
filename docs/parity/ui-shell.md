# Parity: ui-shell (`muxus-ui`, `muxus-app`)

`muxus-ui` is the MUI look of `client/src/theme.ts` in GPUI: theme tokens,
bundled fonts and icons, and the shared widgets. `muxus-app` is the main
window: its library holds the UI-free logic (host catalog model, sidebar
tree, tabs and pane layout, keymap) and the `muxus` binary holds the GPUI
views of the window chrome.

```bash
CARGO_TARGET_DIR=target cargo test -p muxus-app -p muxus-ui
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
| Menu / MenuItem / ListItemIcon / chord hints / divider | MuiMenu override, MenuList | done | paper, border, shadow, 34.3px items, 36px icon column; `menu::popup` is modal (a backdrop closes it on any click outside, so hover cards stay hidden), anchored below its button or at the pointer, and kept 16px inside the window (`marginThreshold`); menus draw on `MENU_LAYER` above dialogs (a menu below its dialog was hidden yet took clicks) and a select's list or an autocomplete is non-modal (`Popup::non_modal`); compared with Electron's host, folder, panel, tab and appearance menus. Keyboard (`menu_keyboard`, tests `tests/menu_keyboard.rs`): an open modal menu takes the focus on its selected item, else its first (`autoFocusItem`, unless the menu focused a field of its own); the arrows move through the pressable items and wrap, Home/End jump, typing jumps to the next label starting with it (`textCriteria`, 500 ms), Enter or Space presses, Escape and Tab close it, and the focus goes back to what had it (a dialog opened from an item goes back there too). The focused item shows `action.focus` (selected: selected + 0.12). A long menu does not scroll the focused item into view |
| Dialog (paper, title, content, actions, backdrop, focus trap) | MuiDialog override, MUI `FocusTrap` | done | paper (32px margin, 12px corners, divider border, elevation 24), h6 title at 16×24px, content 20×24px, actions 8px apart: checked against the MUI 9.5 sources under theme.ts. `focus::FocusTrap` (in `dialog::modal` and `layer::modal_layer`) moves the keyboard into the dialog as it opens, keeps Tab and Shift+Tab inside it and hands the focus back when it closes. Escape stays with the owner. Not yet pixel-compared with an Electron dialog |
| Tabs / Tab | MuiTabs/MuiTab overrides | done | 40px tall, 12×16px padding, 90px minimum width, weight 550, 2px text.primary indicator with rounded top: checked against the MUI sources; tabs are tab stops with the focus ripple. Not yet pixel-compared |
| Checkbox, Radio, Switch | MUI defaults | done | 9px padding around 22.29px (small 18.57px) icons; switch 58×38 (40×24) with a 20px (16px) thumb sliding 20px (16px), track opacities per mode: checked against the MUI sources. Tab stops; Space or Enter toggles; the focus ripple shows keyboard focus. Not yet pixel-compared |
| Select (closed field and open list) | MUI Select | done | a tab stop: keyboard focus outlines it as focused; Enter, Space and the up and down arrows open it. The open list is the Menu the owner mounts, so the arrows, Home/End and type-ahead walk it from the selected option and Enter picks (see Menu). Settings' own `RowSelect` opens on the arrows as well |
| Chip, LinearProgress, Alert (standard/filled), Badge, Divider | MUI defaults + overrides | done | checked against the MUI sources: chip 24px with 8px (outlined 7px) label padding and 16px corners, weight 500; LinearProgress 4px with 3px corners on text.secondary at 0.15; Alert 6×16px padding, 22px icon 12px before the message, 8px corners. Clickable chips are tab stops with MUI's focus background. Badge is the top bar's `sx` variant only. Not yet pixel-compared |
| Scrollbar | MuiCssBaseline scrollbar styles | done | `scrollbar::vertical`: dragging the thumb scrolls with the grabbed point kept under the pointer; a press on the track pages by 87.5% of the viewport toward it (Chromium's step); `scrollThumbHover` under the pointer and while dragged; the wheel still reaches the content. Holding the track does not repeat |
| CircularProgress | MUI | done | `progress::CircularProgress`, indeterminate (the client uses no determinate one) |
| Autocomplete | MUI Autocomplete, useAutocomplete | done | `autocomplete::Autocomplete`: one value, `freeSolo`, `loading`, clear button and popup indicator (out of the tab order), list under the field as wide as it, at most 40% of the window; click toggles, typing opens and filters (case and accents ignored, `createFilterOptions`), arrows wrap, Page Up/Down jump 5, Home/End (not in freeSolo), Enter picks or commits, Escape closes without reaching the dialog, the pointer moves the highlight. Model unit-tested. Settings → font family uses it; the host editor and management workbench keep their own older free-solo autocompletes over `listbox_popup` |
| Slider | MUI Slider | done | `slider::Slider`: horizontal, one thumb, small and medium; press jumps, drag follows outside the control, arrows/Shift/Page Up/Down/Home/End step (`roundValueToStep`), hover/focus (8px) and pressed (14px) rings. Marks, ranges, vertical sliders and the value label are not ported (the client uses none) |
| Table | MUI Table, TableRow, TableCell, TableSortLabel | done | `table::{TableCell, TableRow, TableSortLabel}` (small/medium padding, the theme's 12px/550 head cells, hover and selected rows) and `SortOrder::request`/`sort_rows` (stable, MUI's sort-request rule). The SFTP panel keeps its own row layout (it predates the widget and is pixel-matched) |
| Keyboard focus (Tab order, focus-visible, Enter/Space) | MUI ButtonBase, FocusTrap, the page's sequential focus navigation | done | every control is a tab stop (`focus::TabStop`); keyboard focus shows MUI's focus ripple (the ink at 0.3, without the slow pulse); Enter and Space press the focused control. Buttons keep the focus where it was on a pointer press (as before); form controls take it. Tab walks windows with the component library's root (settings, SFTP), the main window (`focus::tab_order` on its root; a body focus takes the keyboard when nothing has it) and every modal. A terminal and a remote desktop keep Tab and Shift+Tab (unbound in their key contexts), as xterm.js and the canvas take them; multi-line editors indent with them. The terminal itself is not a tab stop (Tab never walks into it) |

## muxus-app: logic (library, tested)

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Host catalog model | shared/src/api-types.ts, ws-protocol.ts | done | `muxus_app::model` re-exports the `muxus_core::api` types plus small helpers |
| Demo catalog | hack/demo-env.mjs, hack/capture.mjs | done | `model::demo` |
| Host grouping, display names, addresses, search text | host-organization.ts, managed-hosts.ts | done | |
| Saved hosts helpers | saved-hosts.ts | done | display name, address, search, filter, jump hop labels and the export helper `open_ssh_jump_hops` (`openSshJumpHops`: the Settings → Data export's ProxyJump lines and the copied ssh commands); `saved-hosts.test.ts` ported, plus a jump-list test |
| Search scoring, best match | host-search.ts | done | |
| Folder tree, flattening, siblings, paths | host-tree.ts | done | arena-backed |
| Row clicks, tree window, navigation, selection, drag and drop rules | components/sidebar/{row-clicks,tree-window,tree-navigation,tree-selection,tree-dnd}.ts | done | |
| Folder mutations, host details, folder icons, kind icons, folder prefs, live counts, row geometry | components/sidebar/* | done | |
| `localeCompare` | ICU root collation | done | `muxus_core::collate::locale_compare`: ICU4X `icu_collator` with the root collation (what Chromium's ICU uses for English) at `Intl.Collator`'s defaults; only the root's data is linked (+0.32 MB stripped release, against +1.24 MB with every locale's tailoring). Golden test (67 strings sorted by the renderer) plus accents, `ß`, Greek, Cyrillic, kana and other digits, checked against Node's ICU 78 |
| Pane layout | state/workspace-layout.ts | done | |
| Tabs store | state/tabs.ts | done | |
| Session actions (connect host/target, local terminal, launch guard, quick-connect test) | session-actions.ts | done | `state::session_actions` (connect host or target, the repeat-launch guard, `is_quick_connect_target`) and `features::launcher::sessions` (local shells, saved shell profiles); `session-launch-guard.test.ts` ported there. The tab, pane, window, logging and reconnect actions of the same file live with the tab menu and window code (`app.md`); the session itself is the backend's |
| Keymap: chords, bindings, command catalog, hints | keymap/*.ts | done | `runKeyCommand` side effects live in the UI |
| Sidebar width | sidebar-width.ts | done | |

Ported test files: host-tree, host-tree-window, tree-dnd, tree-navigation,
tree-selection, row-clicks, host-search, managed-hosts, host-organization,
folder-mutations, tabs, workspace-layout, keymap, sidebar-width, saved-hosts,
session-launch-guard. Skipped cases (they test modules outside this area):
tabs.test.ts host SFTP actions (4) and confirm dialogs (8); keymap.test.ts
command-menu, send-secret and focus-mode `run()` side effects.

## muxus-app: window chrome (binary)

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| App shell layout (top bar, command bar slot, sidebar left/right, canvas, status bar, focus mode) | layout/AppShell.tsx | done | |
| Top bar (menu, logo, title, workspace button, actions, badge, appearance) | layout/TopBar.tsx | done | every button opens its target: multi-execution control, command buttons dialog, file browser toggle (SSH tabs with SFTP), containers and VMs, forwarding panel with its badge, terminal actions menu, session history, appearance menu, shortcuts, settings, focus mode |
| Window decorations | electron main window (titleBarOverlay) | done | `muxus_ui::widgets::window_frame`, shared by every window: drag, double-click maximize, resize edges, min/max/close drawn as Electron draws them on Linux, Chromium's shadow and rounded top corners on a floating window and no border line (measured in the same compositor; `hack/window_edge.py` guards the outline) |
| Sidebar panel + resize grip | layout/Sidebar.tsx, PanelResizeHandle.tsx | done | pointer resize, double-click reset; the grip takes the focus when pressed, Left/Right move it 20px (toward the panes widens) and Home resets it (`sidebar_panel::keyboard_width`, tested), its line shows on hover, while dragging and with keyboard focus. The main window has no Tab walk outside modals, so the grip is reached by pressing it. The docked file browser and its rail (arrows move between the tabs) are `ui/sftp.rs` |
| Session sidebar (search, quick connect, Local terminal, local shell profiles and WSL distributions, config error alert, selection bar, empty states) | SessionSidebar.tsx, SelectionBar.tsx, local-shell-launchers.ts, LocalShellIcon.tsx | done | `ui/sidebar_shells.rs`: saved profiles then the installed distributions no profile covers (Windows only, `showWslDistributions`), filtered by the search, indented under Local terminal with the command line on hover, the distribution's own icon (its `data:` URL decoded once; not seen on Windows here) and their menu; the quick launcher gets the same distributions. The `~/.ssh/config` problem shows as a warning alert above the tree. `ui/sidebar_selection.rs`: "N selected", Edit…, move, delete, clear; a click beside the rows lets go of the selection where a click selects. Reordering and folder edits wait while an order or folder write is on its way. The bar's "Run script…" belongs to the scripts port |
| Host tree rows, folder rows, indent guides, live dots, hover card | sidebar/{HostTree,HostRow,FolderRow}.tsx | done | windowed (spacers + overscan) |
| Tree keyboard navigation and selection | useTreeKeyboard.ts, HostTree.tsx, HostRow.tsx, FolderRow.tsx | done | `ui/sidebar_keys.rs`: arrows, Home/End, Shift+Arrow ranges, Ctrl/Cmd+A, Escape, Enter and Space as a keyboard click (Shift extends a selection or opens another session, Ctrl toggles), Shift+Space toggles, type-ahead (700 ms buffer, `type_ahead_index`) and Alt+Up/Down reordering a host or folder. A row pressed with the pointer becomes the focused row; the focus ring shows for keyboard focus only. With double-click opening, the opening click lists a host's sessions as the client does. Headless tests in `ui/sidebar_tests.rs` |
| Tree drag and drop | useTreeDnd.ts, HostTree.tsx | done | `ui/sidebar_dnd.rs` on the `tree_dnd` rules: hosts into folders and between siblings, folders into and beside folders, a host of a selection drags the whole selection ("N hosts"), "Drop here for no folder" under the rows while dragging. One listener on the tree resolves the row from geometry (`row_at`, the nearest row within 4px, as `nearestRow`); a collapsed folder opens after 500 ms under the pointer and closes again when the drag ends; drop lines are inset to the row's indent, a folder being dropped into is tinted with the 2px primary edge and an open chevron; draggable hosts show the grab cursor. Headless tests drag a host, a selection, a folder beside another, onto the root, and over a collapsed folder |
| Sidebar context menus (host, folder, panel, local shell) | sidebar/*ContextMenu.tsx | done | every item with the client's enabled states, carried out by `ui/host_actions.rs`: host editor, bulk editor, organize, folder dialog (new, edit, move hosts), launch dialog, deletes behind the client's questions, order writes, SFTP, containers. The local shell menu opens a profile, saves a listed WSL distribution as a profile, and opens Settings → Local shells on that profile ("WSL settings…" for a distribution). The "Run script…" items belong to the scripts port |
| Host sessions menu | sidebar/HostSessionsMenu.tsx | done | `ui/tab_actions.rs`; anchored at the host row's edge; status, details, the last two lines of output, New session; the host's row stays marked (and its hover card away) while the menu shows (`sessionsMenuKey`) |
| Tab strip (tabs, status dot, color flag, pin, close, numbers on Alt, new tab, all-tabs menu, split, zoom, close pane) | components/TabStrip.tsx, tab-drag.ts, tab-transfer.ts, electron/src/tab-detach.ts | done | `ui/panes.rs`, `ui/tab_drag.rs` over `features::notices::tab_strip` (tests: FLIP slots and easing, `pointInsideAnyWindow`, `shouldDetachTabDrag`; shell tests `dragged_tabs_resort_live_and_land_where_the_line_shows`, `a_tab_from_another_window_lands_beside_the_tab_under_the_pointer`). Overflow arrows and the wheel; the active tab is scrolled into view when it or the order changes (instantly, not smoothly); double-click renames; title tooltip; status tooltip except before a connection; login sequence mark (pulsing, step tooltip) and multi-execution mark; tabs are tab stops (Enter/Space activate, inset focus ring) that leave the keyboard with the terminal on a pointer press. Drags: the strip copy dims to 0.4, a drag along its own strip resorts live (paused while tabs slide; pinned and unpinned groups never mix; past the last tab goes last), a drag from another pane shows the 2px insertion line and lands there. Order changes slide (FLIP, 160 ms `cubic-bezier(0.2, 0, 0, 1)`). A tab let go outside the window moves, live session included, to the Muxus window under the pointer, or opens in a window of its own; on Wayland window positions are unknown, so it always gets its own window. Below 560px the split and zoom buttons fold into "More pane actions". Tabs are `clamp(168px, 20vw, 210px)` wide when active and `clamp(128px, 15vw, 190px)` otherwise. The in-tab paste progress is not drawn (see `app.md`) |
| Program status marks on tabs and in the all-tabs menu (ring while working, pulsing question mark while waiting, check or cross until the tab is looked at; tooltip with what the program reported) | components/ProgramStatusBadge.tsx, state/program-status.ts (783bb5f) | done | `ui/program_status.rs` over `muxus_app::state::program_status` (store tests ported). Records live with the window, keyed by tab, so a reconnect keeps them; a new session, RIS or a closed tab clears them, a prompt or the session's end drops work in progress. Looking at a tab is checked before each frame (active tab in an active window). The tooltip is the kit's `Tooltip::card` (detail lines at 0.7 opacity, 300 px wide, not 0.85 and 320 px). Shell test `programs_mark_their_tab_until_it_is_looked_at`. |
| Close the tab when its shell exits cleanly | connection-recovery.ts `closesTabOnExit`, TerminalViewImpl (50a8d19) | done | `muxus_session::recovery::closes_tab_on_exit` (tests ported); the close goes through `request_close_tabs`, so unsaved remote files still ask first. |
| Tab and pane context menus | components/TabStrip.tsx | done | `ui/tab_actions.rs` (tab) and `ui/panes.rs` (pane): rename, edit host, pin, duplicate, open/move to a new window, SFTP, move to a split, reconnect (+ tmux/screen), force reconnect, flags, multi-execution, cancel login sequence, session logging and log file, Send BREAK, send text file, close/others/right; the pane menu's splits (left and up mirrored), zoom, even out, close pane, with their chords |
| Pane canvas, split dividers, zoom, active pane border, dimming | AppShell.tsx PaneCanvas | done | divider drag and double-click reset |
| Empty pane | components/EmptyPane.tsx | done | Local terminal, Saved host (the host picker popover hangs from the card) and Add host (host editor); the cards wrap as `repeat(auto-fit, minmax(132px, 1fr))` does, lift a pixel with elevation 2 on hover, and are tab stops with the 2px focus ring |
| Session views | TerminalView, RemoteDesktopView, ManagementView | done | terminals, remote desktops and management sessions are live views (see `app.md`); the framed empty surface shows only for the frame before a tab's view is created |
| Status bar | components/StatusBar.tsx | done | source messages, item menu and the polled host statistics (see `app.md`) |
| Action bar | components/ActionBar.tsx | done | `features::command_buttons::action_bar`, mounted above or below the panes as the preference says (see `dialogs-a.md`) |
| Toasts | components/ToastHost.tsx | done | filled alert, drawn above dialogs (`TOAST_LAYER`); errors and warnings offer Copy ("Copied" after), a toast with details offers Details/Less unfolding them (mono 11px, at most 240px, scrolling); errors and unfolded toasts wait to be dismissed, the rest hide after five seconds. Features pass details with `kit::show_toast_with_details`; the host editor's range save still folds its detail into the message |
| Global shortcuts | shortcuts.ts | done | every command of the catalog runs (`ui/shortcuts.rs`, `run_ui_command`, `keymap::run`): tabs, panes, dialogs (settings, shortcuts, launcher), sidebar, status bar, focus mode, terminal commands, command buttons. GPUI reports layout keys, not physical `code`s |
| Command+Left/Right line jumps (Ctrl+A / Ctrl+E typed into the active session with its host's keys and echo, mirrored under multi-execution; only from a focused terminal) and Command+Option+arrow pane focus on macOS | keymap/commands.ts, shortcuts.ts (81b8798, 8eac3ca) | done | `terminal.line-start` / `terminal.line-end` with `types_into_session`; the catalog is built per platform (`build_commands_for`), so the macOS keymap tests (`keymap.test.ts` line jumps and pane focus keys) run on Linux. The terminal reports no key releases, so `swallowKeyUp` has nothing to keep away. Not run on a Mac. |
| macOS traffic lights centered in the title bar, focus mode's shorter one included | electron/src/window-placement.ts `trafficLightPosition`, main.ts (50b3815) | done | `window_frame::traffic_light_position` (tests ported) and `place_traffic_lights` from the top bar. The macOS call itself is not compiled here. |
| Ctrl+Shift+I | electron hidden menu (toggleDevTools) | done | gpui-component binds it (Cmd+Alt+I on macOS) to its inspector in debug builds; `muxus_ui::init` unbinds it in the `Terminal` key context so a focused terminal always receives the chord. Electron hands it to xterm.js first too, but opens the developer tools when xterm leaves it unhandled (it encodes the chord only under the kitty keyboard protocol); release builds have no inspector |
| Layout preview | components/LayoutPreview.tsx | done | `features/settings/view/layout_preview.rs` in Settings → Appearance (see `settings.md`); the 220 ms slide between layouts is not animated |
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
keyboard command. The window's own chrome (tab strip drags and slides, menus and Tab by
keyboard, toasts) is complete as listed above.
