//! What the user does to a terminal from menus and the keyboard: the
//! terminal's context menu (`TerminalViewImpl.tsx`), the top bar's terminal
//! actions menu (`TopBar.tsx`), the `terminal.*` keyboard commands
//! (`keymap/commands.ts`), clipboard paste with its preview, and exports.

use muxus_app::export::export_filename;
use muxus_app::keymap::bindings::KeybindingOverrides;
use muxus_app::keymap::hints::chord_label;
use muxus_app::model::{ProfileKind, SessionProfile};
use muxus_app::session::LinkInput;
use muxus_app::state::tabs::TabStatus;
use muxus_core::protocol::TerminalClientMessage;
use muxus_term_view::muxus_term::serialize::serialize_as_html;
use muxus_ui::gpui_kit::{
    AppContext, ClipboardEntry, Context, Div, Entity, ParentElement, Styled, Window, div,
};
use muxus_ui::text::TextStyleExt;
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::MenuItem;
use muxus_ui::widgets::menu::{self, menu_divider};

use crate::ui::paste_dialog::{PasteDialog, PasteDialogEvent};

/// The context menu's shortcut text: a caption 24px after the label.
fn caption_hint(
    text: &'static str,
    theme: &muxus_ui::theme::Theme,
) -> muxus_ui::gpui_kit::AnyElement {
    use muxus_ui::gpui_kit::IntoElement;
    let caption = theme.typography.caption;
    div()
        .ml(muxus_ui::gpui_kit::px(24.))
        .child(
            muxus_ui::label::label(text)
                .size(caption.size)
                .line_height(caption.line_height)
                .tracking(caption.letter_spacing)
                .color(theme.palette.text.secondary)
                .no_truncate(),
        )
        .into_any_element()
}
use crate::ui::shell::AppShell;

impl AppShell {
    /// A dialog has the keyboard: menus and terminal focus stay away.
    pub fn modal_open(&self) -> bool {
        self.paste_dialog.is_some()
            || self.confirm.is_some()
            || self.prompt_dialog.is_some()
            || self.modal_overlay_open()
            || self.features.vault_blocking
    }

    /// `pasteFromClipboard`: text is pasted (previewed when it spans lines),
    /// an image becomes a path the shell can read.
    pub fn paste_clipboard(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            return;
        };
        if let Some(text) = item.text() {
            self.paste_text(tab_id, &text, cx);
            return;
        }
        if item
            .entries()
            .iter()
            .any(|entry| matches!(entry, ClipboardEntry::Image(_)))
            && let Some(view) = self.terminal_of(tab_id)
        {
            // The view asks for the image path the same way its own paste does.
            view.update(cx, |_, cx| {
                cx.emit(muxus_term_view::TerminalViewEvent::ClipboardWithoutText)
            });
        }
    }

    /// Show the paste preview for a multi-line paste into `tab_id`.
    pub fn open_paste_dialog(
        &mut self,
        tab_id: &str,
        text: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(view) = self.terminal_of(tab_id)
            && view.read(cx).is_search_open()
        {
            view.update(cx, |view, cx| view.close_search(window, cx));
        }
        let pacing = self.paste_pacing(tab_id);
        let (family, size, line_height) = (
            self.prefs.font_family.clone(),
            self.prefs.mono_font_size as f32,
            self.prefs.line_height as f32,
        );
        let tab = tab_id.to_string();
        let dialog = cx.new(|cx| {
            PasteDialog::new(tab, text, pacing, 0, &family, size, line_height, window, cx)
        });
        let subscription = cx.subscribe_in(&dialog, window, Self::on_paste_dialog_event);
        self.menu = None;
        self.paste_dialog = Some((dialog, subscription));
        cx.notify();
    }

    fn on_paste_dialog_event(
        &mut self,
        dialog: &Entity<PasteDialog>,
        event: &PasteDialogEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let tab_id = dialog.read(cx).tab_id.clone();
        self.paste_dialog = None;
        if let PasteDialogEvent::Confirm {
            text,
            line_delay_ms,
        } = event
        {
            self.paste_into_session(&tab_id, text, Some(*line_delay_ms), cx);
        }
        self.focus_terminal(&tab_id, window, cx);
        cx.notify();
    }

    /// Save the terminal's text (or HTML with colors) through a save dialog.
    pub fn export_terminal(&mut self, tab_id: &str, html: bool, cx: &mut Context<Self>) {
        let Some(view) = self.terminal_of(tab_id) else {
            return;
        };
        let title = self
            .tabs
            .tab(tab_id)
            .map(|tab| tab.title.clone())
            .unwrap_or_default();
        let content = {
            let view = view.read(cx);
            if html {
                let settings = view.settings();
                serialize_as_html(view.emulator(), &settings.font_family, settings.font_size)
            } else {
                view.emulator().buffer_text()
            }
        };
        let name = export_filename(&title, if html { "html" } else { "txt" });
        let directory = std::env::var_os("HOME")
            .map(std::path::PathBuf::from)
            .map(|home| {
                let downloads = home.join("Downloads");
                if downloads.is_dir() { downloads } else { home }
            })
            .unwrap_or_else(std::env::temp_dir);
        let chosen = muxus_app::util::file_dialog::ask_save_path(&directory, &name, cx);
        cx.spawn(async move |this, cx| {
            let Some(path) = chosen.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            if let Err(err) = std::fs::write(&path, content) {
                let _ = this.update(cx, |this, cx| {
                    this.show_toast(Status::Error, format!("Could not save the file: {err}"), cx)
                });
            }
        })
        .detach();
    }

    /// The terminal's right-click menu (`rightClickAction: menu`).
    pub fn terminal_menu(&mut self, tab_id: &str, selection: &str, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let id = tab_id.to_string();
        let selection = selection.to_string();
        let has_selection = !selection.is_empty();
        menu::paper(&theme)
            .child(
                MenuItem::new("terminal-copy", "Copy")
                    .icon("ContentCopy")
                    .trailing(caption_hint("Ctrl+Shift+C", &theme))
                    .disabled(!has_selection)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.copy_text(selection.clone(), cx);
                            if let Some(view) = this.terminal_of(&id) {
                                view.update(cx, |view, cx| view.clear_selection(cx));
                            }
                            this.close_terminal_menu(&id, window, cx);
                        }
                    })),
            )
            .child(
                MenuItem::new("terminal-paste", "Paste")
                    .icon("ContentPaste")
                    .trailing(caption_hint("Ctrl+Shift+V", &theme))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.close_terminal_menu(&id, window, cx);
                            this.paste_clipboard(&id, cx);
                        }
                    })),
            )
            .child(menu_divider(&theme))
            .child(
                MenuItem::new("terminal-select-all", "Select all")
                    .icon("SelectAll")
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            if let Some(view) = this.terminal_of(&id) {
                                view.update(cx, |view, cx| view.select_all(cx));
                            }
                            this.close_terminal_menu(&id, window, cx);
                        }
                    })),
            )
            .child(
                MenuItem::new("terminal-find", "Find")
                    .icon("Search")
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.menu = None;
                            if let Some(view) = this.terminal_of(&id) {
                                view.update(cx, |view, cx| view.open_search(window, cx));
                            }
                            cx.notify();
                        }
                    })),
            )
            .child(menu_divider(&theme))
            .child(
                MenuItem::new("terminal-export-text", "Export as text")
                    .icon("DescriptionOutlined")
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.export_terminal(&id, false, cx);
                            this.close_terminal_menu(&id, window, cx);
                        }
                    })),
            )
            .child(
                MenuItem::new("terminal-export-html", "Export as HTML")
                    .icon("CodeOutlined")
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.export_terminal(&id, true, cx);
                            this.close_terminal_menu(&id, window, cx);
                        }
                    })),
            )
            .child(menu_divider(&theme))
            .child(
                MenuItem::new("terminal-clear", "Clear scrollback")
                    .icon("DeleteSweepOutlined")
                    .trailing(caption_hint("Ctrl+Shift+K", &theme))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        if let Some(view) = this.terminal_of(&id) {
                            view.update(cx, |view, cx| view.clear(cx));
                        }
                        this.close_terminal_menu(&id, window, cx);
                    })),
            )
    }

    /// `closeCtxMenu`: the menu goes and the terminal takes the keyboard back.
    fn close_terminal_menu(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.menu = None;
        self.focus_terminal(tab_id, window, cx);
        cx.notify();
    }

    /// The top bar's "Terminal actions" menu, for the active tab.
    pub fn terminal_actions_menu(&mut self, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let overrides = KeybindingOverrides::default();
        let chord = |id: &str| chord_label(id, &overrides);
        let active = self.tabs.active_id.clone().unwrap_or_default();
        let tab = self.tabs.tab(&active).cloned();
        let zoom = self
            .terminal_of(&active)
            .map(|view| view.read(cx).zoom_percent())
            .unwrap_or(100);
        let supports_break = matches!(
            tab.as_ref()
                .and_then(|tab| tab.profile.as_ref())
                .map(SessionProfile::kind),
            Some(ProfileKind::Serial | ProfileKind::Telnet | ProfileKind::Ssh)
        );
        let can_break = supports_break
            && tab.as_ref().is_some_and(|tab| {
                tab.status == TabStatus::Connected
                    || (tab.status == TabStatus::Connecting && tab.conn_id.is_some())
            });
        let mut paper = menu::paper(&theme)
            .child(
                MenuItem::new("find", "Find")
                    .icon("SearchOutlined")
                    .chord(chord("terminal.find"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        if this.tabs.active_id.is_some() {
                            this.tabs.request_search();
                        }
                        cx.notify();
                    })),
            )
            .child(
                MenuItem::new("select-all", "Select all")
                    .icon("SelectAll")
                    .chord(chord("terminal.select-all"))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.menu = None;
                        if let Some(view) = this.active_terminal() {
                            view.update(cx, |view, cx| view.select_all(cx));
                            if let Some(id) = this.tabs.active_id.clone() {
                                this.focus_terminal(&id, window, cx);
                            }
                        }
                        cx.notify();
                    })),
            )
            .child(
                MenuItem::new("copy-all", "Copy all output")
                    .icon("ContentCopy")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        if let Some(view) = this.active_terminal() {
                            let text = view.read(cx).emulator().buffer_text();
                            this.copy_text(text, cx);
                            this.show_toast(Status::Success, "Terminal output copied.", cx);
                        }
                        cx.notify();
                    })),
            )
            .child(menu_divider(&theme))
            .child(
                MenuItem::new("export-text", "Export as text")
                    .icon("DescriptionOutlined")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        if let Some(id) = this.tabs.active_id.clone() {
                            this.export_terminal(&id, false, cx);
                        }
                        cx.notify();
                    })),
            )
            .child(
                MenuItem::new("export-html", "Export as HTML (colors)")
                    .icon("CodeOutlined")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        if let Some(id) = this.tabs.active_id.clone() {
                            this.export_terminal(&id, true, cx);
                        }
                        cx.notify();
                    })),
            );
        if let Some(tab) = tab.as_ref().filter(|tab| tab.logging_enabled.is_some()) {
            paper = paper.child(menu_divider(&theme));
            for item in self.logging_menu_items(tab, cx) {
                paper = paper.child(item);
            }
        }
        if supports_break {
            paper = paper.child(menu_divider(&theme)).child(
                MenuItem::new("send-break", "Send BREAK")
                    .icon("PowerInputOutlined")
                    .chord(chord("terminal.send-break"))
                    .disabled(!can_break)
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        if let Some(id) = this.tabs.active_id.clone() {
                            this.send_break(&id, cx);
                        }
                        cx.notify();
                    })),
            );
        }
        paper
            .child(menu_divider(&theme))
            .child(
                MenuItem::new("clear", "Clear scrollback")
                    .icon("DeleteSweepOutlined")
                    .chord(chord("terminal.clear"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.menu = None;
                        if let Some(view) = this.active_terminal() {
                            view.update(cx, |view, cx| view.clear(cx));
                        }
                        cx.notify();
                    })),
            )
            .child(menu_divider(&theme))
            // The zoom items keep the menu open, so the percentage can be watched.
            .child(
                MenuItem::new("zoom-in", "Zoom in")
                    .icon("ZoomIn")
                    .chord(chord("terminal.zoom-in"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(view) = this.active_terminal() {
                            view.update(cx, |view, cx| view.zoom_in(cx));
                        }
                        cx.notify();
                    })),
            )
            .child(
                MenuItem::new("zoom-out", "Zoom out")
                    .icon("ZoomOut")
                    .chord(chord("terminal.zoom-out"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(view) = this.active_terminal() {
                            view.update(cx, |view, cx| view.zoom_out(cx));
                        }
                        cx.notify();
                    })),
            )
            .child(
                MenuItem::new("zoom-reset", "Reset zoom")
                    .leading(
                        div()
                            .ui_font(12., 400.)
                            .text_color(theme.palette.action.active)
                            .child(format!("{zoom}%")),
                    )
                    .chord(chord("terminal.zoom-reset"))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(view) = this.active_terminal() {
                            view.update(cx, |view, cx| view.zoom_reset(cx));
                        }
                        cx.notify();
                    })),
            )
    }

    /// Run a `terminal.*` (or other UI-side) keyboard command. `None` when
    /// the id is not one of them; `Some(false)` leaves the key to the terminal.
    pub fn run_ui_command(
        &mut self,
        id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<bool> {
        let active = self.tabs.active_id.clone();
        let view = self.active_terminal();
        let handled = match id {
            "terminal.copy" => match &view {
                Some(view) if view.read(cx).has_selection() => {
                    let text = view.read(cx).selection_text();
                    self.copy_text(text, cx);
                    true
                }
                _ => false,
            },
            "terminal.paste" => match &active {
                Some(id) if view.is_some() => {
                    let id = id.clone();
                    self.paste_clipboard(&id, cx);
                    true
                }
                _ => false,
            },
            "terminal.select-all" => match &view {
                Some(view) => {
                    view.update(cx, |view, cx| view.select_all(cx));
                    true
                }
                None => false,
            },
            "terminal.clear" => match &view {
                Some(view) => {
                    view.update(cx, |view, cx| view.clear(cx));
                    true
                }
                None => false,
            },
            "terminal.zoom-in" | "terminal.zoom-out" | "terminal.zoom-reset" => match &view {
                Some(view) => {
                    view.update(cx, |view, cx| match id {
                        "terminal.zoom-in" => view.zoom_in(cx),
                        "terminal.zoom-out" => view.zoom_out(cx),
                        _ => view.zoom_reset(cx),
                    });
                    true
                }
                None => false,
            },
            "terminal.send-break" => match &active {
                Some(id) => {
                    let id = id.clone();
                    self.send_break(&id, cx)
                }
                None => false,
            },
            "terminal.cancel-login-sequence" => match &active {
                Some(id)
                    if self
                        .tabs
                        .tab(id)
                        .is_some_and(|tab| tab.login_sequence.is_some()) =>
                {
                    let id = id.clone();
                    self.send_to_session(
                        &id,
                        LinkInput::message(TerminalClientMessage::CancelLoginSequence),
                        cx,
                    )
                }
                _ => false,
            },
            "tab.close" => match &active {
                Some(id) => {
                    let id = id.clone();
                    self.request_close_tabs(vec![id], window, cx);
                    true
                }
                None => false,
            },
            // The focused pane, or the tab when the canvas has a single pane.
            "pane.close" => {
                if matches!(
                    self.tabs.root,
                    muxus_app::state::workspace_layout::PaneNode::Pane(_)
                ) {
                    match &active {
                        Some(id) => {
                            let id = id.clone();
                            self.request_close_tabs(vec![id], window, cx);
                            true
                        }
                        None => false,
                    }
                } else {
                    let pane = self.tabs.active_pane_id.clone();
                    self.request_close_pane(&pane, window, cx);
                    true
                }
            }
            "terminal.command-menu" => self.open_command_menu(window, cx),
            "terminal.send-secret" => view.is_some() && self.open_send_secret_menu(window, cx),
            "terminal.multi-exec" => {
                self.toggle_multi_exec(cx);
                true
            }
            "terminal.force-reconnect-all" => self.request_force_reconnect_all(window, cx),
            "app.status-bar" => {
                self.prefs.show_status_bar = !self.prefs.show_status_bar;
                self.prefs_changed(cx);
                true
            }
            _ => return None,
        };
        Some(handled)
    }
}

impl AppShell {
    /// `requestForceReconnectAll`: replace every remote connection, after asking.
    pub fn request_force_reconnect_all(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let remote: Vec<_> = self
            .tabs
            .tabs
            .iter()
            .filter(|tab| muxus_app::state::tabs::is_remote_session_tab(tab))
            .collect();
        if remote.is_empty() {
            return false;
        }
        let count = remote.len();
        let live = remote
            .iter()
            .filter(|tab| tab.status != muxus_app::state::tabs::TabStatus::Closed)
            .count();
        let plural = |n: usize| if n == 1 { "" } else { "s" };
        self.open_confirm(
            crate::ui::confirm::ConfirmRequest {
                title: format!("Force reconnect {count} remote session{}?", plural(count)),
                description: Some(if live > 0 {
                    format!(
                        "This ends {live} live or connecting session{} and starts fresh connections in the same tabs. Remote programs survive only when they run in tmux or screen.",
                        plural(live)
                    )
                } else {
                    "This starts fresh connections for every remote tab.".into()
                }),
                confirm_label: "Force reconnect all".into(),
                destructive: live > 0,
                checkbox: None,
                action: crate::ui::confirm::ConfirmAction::ForceReconnectAll,
            },
            window,
            cx,
        );
        true
    }
}
