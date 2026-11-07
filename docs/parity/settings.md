# Parity: settings (`muxus-app/src/features/settings`)

The settings window, its twelve sections and the keyboard shortcuts editor:
`client/src/layout/SettingsWindow.tsx`, `components/SettingsDialog.tsx`
(`SettingsBody` and every section), `SettingsLayout.tsx`, the section
components and `components/ShortcutsDialog.tsx`, plus the logic they use.
Preferences are owned by `muxus_session::Backend` (`prefs`, `set_prefs`,
`client_state_item`).

```bash
CARGO_TARGET_DIR=target-ui-a cargo test -p muxus-app --lib features::settings   # 117 tests
CARGO_TARGET_DIR=target-ui-a cargo build -p muxus-app --example settings
hack/shot.sh --size 1000x720 --env MUXUS_WINDOW=fullscreen --env MUXUS_THEME=dark \
  --env MUXUS_SETTINGS_SECTION=terminal out.png -- target-ui-a/debug/examples/settings
```

The example (`examples/settings.rs`) starts a sandboxed backend (scratch data folder and HOME),
seeds the docs screenshots' preferences (`hack/capture.mjs` BASE_PREFS plus its highlighting
rules), the demo hosts and tunnels of `hack/demo-env.mjs` and the demo's logging policy, then
opens the window. `MUXUS_SETTINGS_SECTION`, `MUXUS_SETTINGS_SCROLL`, `MUXUS_SETTINGS_SHORTCUTS=1`,
`MUXUS_THEME` and `MUXUS_DATA_DIR` choose what it shows.

## Hook points for the app

| API | What it does |
|---|---|
| `SettingsPrefs::init(backend, prefs, cx)` | Installs the shared preferences entity (a GPUI global). Call once at startup with `backend.prefs()`. Calling it again replaces the state and notifies listeners. |
| `SettingsPrefs::global(cx)` / `.read(cx).prefs()` | The current `PrefsState` for every window. |
| `SettingsPrefs::update(cx, \|prefs\| ...)` | Changes preferences: listeners hear at once, the state is written through `Backend::set_prefs` in the background (one write at a time, always the newest). |
| `cx.subscribe(&prefs_entity, \|_, _, event: &PrefsChanged, cx\| ...)` | Live application. `PrefsChanged { previous, current }` with `changed(\|p\| p.font_family.clone())`. Theme mode, the X11 switches (`put_x11_settings`) and the debug log level (`put_log_settings`) are applied by the entity itself, as `App.tsx` does. `apply_theme(cx)` re-resolves `System` after an OS appearance change. |
| `SettingsPrefs::reload(cx)` | Re-reads `client-state.json` (another process wrote it; a restore with preferences calls it). |
| `open_settings_window(Some(SettingsTarget { section, item }), cx)` | `showSettingsWindow`: focuses the open window and shows the target, or opens it where it was left (`settings-window-state.json`, moved onto the display it overlaps most) or centred on the active window, 1000×720 by default, min 560×420. Returns `(AnyWindowHandle, Entity<SettingsWindow>)`. |
| `open_shortcuts(cx)` | Opens the settings window at Keyboard with the shortcuts editor over it. |
| `ShortcutsDialog::new(prefs_entity, window, cx)` + `ShortcutsEvent::Close` | The editor as an overlay entity any window can mount (the top bar's keyboard button). While it records, `chord_capture_active(cx)` is true and the window's shortcut dispatcher must stand down. |
| `set_settings_hooks(SettingsHooks { .. }, cx)` | What settings ask of the rest of the app: `open_log_viewer` (Debug → View logs), `import_sessions(SessionImportSource::{MobaXterm, SecureCrt})` (Backup & data), `desktop_update` (the in-app updater's state and actions for About). A missing hook shows a toast. |
| `SettingsWindow::pin_x11_status` | Shows a fixed X11 status (previews and tests). |

## Status

| Feature | TypeScript source | Status | Note |
|---|---|---|---|
| Settings window: own native window, title bar as drag region, client-side frame and controls, Esc closes, unsaved logging edits hold the window (prompt "Discard changes" / "Keep editing") | layout/SettingsWindow.tsx, electron/src/main.ts (`createSettingsWindow`) | done | Frame code duplicates the binary's `ui/window_frame.rs` (`view/frame.rs`) until the frame moves into the library. Wayland ignores window positions, so only the size is restored there. |
| Placement: opens centered over the window it was opened from, kept on that window's display, at the size it was last left (and maximized if it was) | electron/src/window-placement.ts, main.ts (v0.15, 4f18db5) | done | `muxus_store::prefs::window_state::window_bounds(size, owner, work_areas)`; `settings-window-state.json` keeps `{ width, height, maximized }` (positions saved by earlier versions are read and dropped); window-placement.test.ts ported |
| Search settings: field above the section list, ranked results with section › heading, Ctrl/Cmd+F, pick opens the section, scrolls to the setting and outlines it for 2.5 s | settings-search.ts, SettingsSearch.tsx, SettingsDialog.tsx, SettingsLayout.tsx (v0.15, 90d819f) | partial | Index and ranking in `search.rs` with settings-search.test.ts ported, including the check that every row and group of the views is indexed (it reads the Rust views); the view is `view/search.rs` on the kit's `Autocomplete` (unfiltered, auto-highlight, no popup button, wider list, clear on pick/Escape). In a narrow window (below 600px) the field sits above the section picker. |
| Section navigation, deep links (`section` + `item`), leave-logging confirmation, unsaved dot with tooltip | SettingsDialog.tsx `useSettingsNavigation`, `SettingsBody` | done | `section::SettingsNavigation` (tested). Below 600px (MUI's `sm`) the section list gives way to the search field and a full-width section picker above the page, whose side padding drops to 16px. |
| Live cross-window preference sync | zustand persist + desktop bridge | done | one in-process entity; see hook points |
| Settings layout language (page, group, row, row select, status text) | SettingsLayout.tsx | done | `view/kit.rs` |
| Appearance: theme, interface scale, layout toggles + live preview, status bar items, terminal colors (scheme pickers, the dark one listing dark schemes first, text/background color with "Use scheme color"), terminal font, sliders, split panes | SettingsDialog.tsx `AppearanceSection`, `LayoutSettings`, LayoutPreview.tsx, TerminalSchemeSelect.tsx (`firstMode`, v0.15 4a9baa3) | done | `view/scheme.rs::terminal_scheme_groups(custom, SchemeMode)`, terminal-scheme-groups.test.ts ported. Interface scale is stored; scaling the main window is the app's. The font family is a free-solo `Autocomplete` over the bundled, installed and generic families (`terminal_font_families`); installed fonts drive the warning and the description. The sliders are the kit's `Slider` (small, 180px), stepping by keyboard too. |
| Custom color schemes: new from a copy, import one or more files (Windows Terminal, iTerm2, Muxus) reported in one message, export one/all, rename, delete, color editor with preview | CustomTerminalSchemeSettings.tsx, custom-schemes.ts (v0.15 d11e94d) | done | file logic is `muxus-term::custom_schemes`; the multi-file import (a file that cannot be read does not stop the others, schemes whose colors are present are skipped, one summary) is `muxus-term::scheme_import` with its tests ported |
| Color wells | `<input type="color">` | done (replaced) | the browser opens the system dialog; here a popover with a saturation/value square, hue strip and hex field |
| Terminal: cursor, clipboard and mouse, paste pacing, buffer and rendering | `TerminalSection` | done | "GPU renderer (WebGL)" defaults on (v0.15 622b232: `webglRenderer` default in `muxus_store::prefs`; saved values are kept). GPUI always paints on the GPU, so the switch only round-trips the preference. |
| Local shells: default profile, automatic shell, saved profiles (add, edit, delete, outline when opened for one), WSL distributions on Windows | LocalShellProfilesSection.tsx | done | profile helpers and their tests are `muxus-local::profile`; WSL icons (`LocalShellIcon`) not shown here (the sidebar's shell rows show them) |
| Session logging: default and local policies, log files, storage and retention, save bars, drafts that follow refetches | `SessionLoggingSection`, SessionLoggingPolicyFields.tsx, settings-draft.ts, session-logging-policy.ts | done | `draft.rs` (tests) |
| Highlighting: global rules, reusable profiles (new, built-in add/reset, import, export, rename, delete), rule editor (colors popover, flags, move, delete, JSON editor with Ctrl+Enter) | HighlightProfilesSection.tsx, KeywordHighlightRulesEditor.tsx | done | slow-pattern notifications are not subscribed (the error shows on the next render) |
| Behavior | `BehaviorSection` | done | The Links group (system handler for ssh:// and telnet://) is backed by `muxus_session::links` (`Backend::link_handlers`, `register_link_handler`); see `desktop.md`. Tabs has **Close the tab when its shell exits** (50a8d19) and **Show program status on tabs** (783bb5f); Hosts sidebar has **Find containers, clusters and VMs** and **Refresh containers, clusters and VMs** (`environments.md`), in the TypeScript order; all four are in the settings search index. |
| X11 forwarding | X11Section.tsx | done | |
| Keyboard section | `KeyboardSection` | done | |
| Keyboard shortcuts editor: search, record (add/replace), remove, reset one, restore defaults, conflict warning, gestures and editor keys | ShortcutsDialog.tsx | done | uses `crate::keymap::keystroke::chord_event`; the chip delete icon is drawn (MUI's `Cancel` glyph is not bundled). Command buttons with a shortcut follow in a section of their own, one chord each, checked with `command_button_shortcut_conflict` |
| Confirm pastes with look-alike characters | SettingsDialog.tsx (`pasteWarnLookalikes`) | done | Terminal section switch under "Confirm multiline pastes" |
| Passwords: vault status and actions, master-password dialog (create, change, repair, unlock, policy), saved passwords (view/edit, forget), secrets (add, view/edit, delete with command-button and login-sequence warnings), remember-by-default | PasswordVaultSection.tsx, VaultSecretDialog.tsx | done | |
| Backup & data: summary, create backup, restore review dialog (categories, conflict strategy, ProxyCommand warning), drop a file, OpenSSH export, import hooks | DataTransferSection.tsx, data-transfer.ts | done | logic in `data_transfer` (71 tests, every data-transfer.test.ts case); dates use en-US formatting; the MobaXterm/SecureCRT dialogs are the importer's (hook) |
| Debug: verbose logging, view logs (hook), export logs | `DebugSection` | done | `files.rs` (export name, log lines; tests) |
| About: hero, update check, in-app updater row, installation facts, copy diagnostics, links | AboutSection.tsx, DesktopUpdateControls.tsx, app-facts.ts | done | the updater itself is not built; its row shows when `SettingsHooks::desktop_update` reports a state |
| macOS app menu: About Muxus opens Settings → About | electron/src/main.ts `buildMenu` (v0.15 7a4be79) | done (not run) | `features::desktop::app_menu`: About, Services, Hide, Hide Others, Show All, Quit (⌘H, ⌥⌘H, ⌘Q), set with `cx.set_menus` on macOS only. Compiled and its structure tested on Linux; never run on a Mac. Electron's other stock menus (File, Edit, View, Window) have no Rust counterpart. |
| Toasts, confirmations | ToastHost.tsx, DialogHost.tsx | done | per window, like the client |
| Vault startup coordination | password-vault-startup.ts | done | `vault_startup.rs` (test ported) |
| Vault secret helpers | vault-secrets.ts | partial | find/missing/buttons using/filter/`secretTerminalIds`/`quotedList` + tests; sending a secret belongs to the command buttons feature |
| Keyboard focus order (Tab between controls) | MUI | done | Tab and Shift+Tab walk inputs, buttons, switches, checkboxes, sliders, toggle groups and selects in paint order (the window's component root); keyboard focus shows MUI's focus ripple (a 2px primary outline on selects); Enter or Space presses the focused control; the window's dialogs trap the focus |

## Screenshots

References are fresh captures of the current client's settings window (headless Chrome,
1000×720 at 2×, the window page opened through its `#launch=` fragment, the demo sandbox),
because the docs' `settings*.png` / `shortcuts*.png` are from July and show the old settings
dialog over the main window (1420×880): they cannot be compared with the window. The Rust
window runs fullscreen on a 1000×720 virtual monitor in the same capture harness.
Share of device pixels differing by more than 24/255:

| Screen | Light | Dark |
|---|---|---|
| Appearance | 3.26% | 3.51% |
| Terminal | 2.16% | 2.19% |
| Local shells | 1.71% | 1.89% |
| Session logging | 2.56% | 2.73% |
| Highlighting | 2.31% | 2.35% |
| Behavior | 2.76% | 3.33% |
| X11 forwarding | 2.26% | 2.26% |
| Keyboard | 1.78% | 1.76% |
| Passwords | 1.88% | 1.93% |
| Backup & data | 3.88% | 4.33% |
| Debug | 1.58% | 1.67% |
| About | 2.63% | 2.74% |
| Shortcuts editor | 1.92% | 1.86% |

What remains:
- Glyph rasterization (swash vs FreeType), as in the main window.
- Row heights: Chrome accumulates fractional heights (61.28px rows); GPUI rounds text line boxes up
  to device pixels, so line heights are held to the half-pixel grid (`kit::snap`) and a long page
  drifts by up to 2px.
- The content scrollbar: headless Chrome hides scrollbars; the window draws the themed thumb.
- Browser-only content in the references: About shows "Web app in your browser" and Server rows
  (the desktop app shows "Desktop app"); the font hint is the browser's.
- Backup & data: two descriptions wrap one word apart (text widths differ by a few pixels).

## Known differences

- Desktop app behaviour is the reference where the browser differs (font list, installation facts).
- The bundled X server is detected on Windows only (`muxus-ssh`), while the TypeScript demo server
  reports it on Linux too; the example pins that status to match the reference.

## Attribution

No third-party code was adapted. Everything here is ported from Muxus itself (MIT).

## Integration pass

- The settings window is opened from the main window (top bar, `app.settings`, the sidebar's
  local shell settings), its hooks are registered at startup (log viewer, session import;
  desktop updates only with `MUXUS_UPDATE_FEED`), and the main window stands down while the
  shortcuts editor records a chord.
- Its frame is `muxus_ui::widgets::window_frame` (the local `view/frame.rs` copy is gone).
- `view/fields.rs` is now a builder over the kit's one outlined field, `FormField` (label in
  the notch, helper/error, adornments, a monospace face, multi-line text areas). End
  adornments sit 8px after the input, as MUI's `InputAdornment` does.
- Content colors (scheme fallbacks, the color picker's square) are in `settings/palette.rs`;
  a new keyword rule's colors are `KeywordHighlightRule::DEFAULT_*` (shared with the host
  editor); the session logging drafts are the shared `model::session_logging`.
