//! What a tab's context menu and the top bar's terminal menu do (`TabStrip.tsx`,
//! `TopBar.tsx`, `session-actions.ts`): renaming, session logging, reconnects and the
//! per-tab switches.

use muxus_app::features::notices::tab_strip::{RenameTabEvent, open_rename_tab_dialog};
use muxus_app::hosts::managed::editable_managed_host_for_profile;
use muxus_app::model::{ProfileKind, SessionProfile};
use muxus_app::session::LinkInput;
use muxus_app::state::tabs::{
    ReattachMode, TabPatch, TabStatus, TerminalTab, closable_tab_ids_to_right,
    is_remote_session_tab,
};
use muxus_core::protocol::TerminalClientMessage;
use muxus_ui::color::try_hex;
use muxus_ui::gpui_kit::{
    Context, Div, ElementId, InteractiveElement, ParentElement, StatefulInteractiveElement, Styled,
    Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::text::TextStyleExt;
use muxus_ui::theme::{ActiveTheme, Status, TAB_FLAG_COLORS};
use muxus_ui::widgets::menu::{self, MenuItem, menu_divider};
use muxus_ui::widgets::{AnchorBounds, Tooltip};

use muxus_app::features::host_editor::view::HostEditorTarget;
use muxus_app::state::workspace_layout::PaneDirection;

use crate::ui::shell::AppShell;

/// Which logging switch a menu item flips.
#[derive(Clone, Copy, Debug)]
pub enum LoggingChange {
    Enabled(bool),
    Paused(bool),
    CaptureInput(bool),
    StopFile,
}

impl AppShell {
    /// `setRenaming`: the rename dialog for a tab.
    pub fn open_rename_tab(&mut self, tab_id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        let dialog = open_rename_tab_dialog(&tab.id, &tab.title, window, cx);
        self.mount_overlay(
            "rename-tab",
            dialog,
            window,
            cx,
            |this, _, event: &RenameTabEvent, window, cx| {
                if let RenameTabEvent::Rename { tab_id, title } = event {
                    this.tabs.update(
                        tab_id,
                        TabPatch {
                            title: Some(title.clone()),
                            ..Default::default()
                        },
                    );
                    // The active history record follows the tab's name.
                    this.send_to_session(
                        tab_id,
                        LinkInput::message(TerminalClientMessage::SetTitle {
                            title: title.clone(),
                        }),
                        cx,
                    );
                    this.tabs_changed(cx);
                }
                this.close_overlay("rename-tab", window, cx);
            },
        );
    }

    /// `terminal.setLogging`: one switch of the session's logging.
    pub fn set_logging(
        &mut self,
        tab_id: &str,
        switch: LoggingChange,
        cx: &mut Context<Self>,
    ) -> bool {
        let (enabled, paused, capture_input, log_to_file) = match switch {
            LoggingChange::Enabled(on) => (Some(on), None, None, None),
            LoggingChange::Paused(on) => (None, Some(on), None, None),
            LoggingChange::CaptureInput(on) => (None, None, Some(on), None),
            LoggingChange::StopFile => (None, None, None, Some(false)),
        };
        self.send_to_session(
            tab_id,
            LinkInput::message(TerminalClientMessage::SetLogging {
                enabled,
                paused,
                capture_input,
                log_to_file,
                log_file_path: None,
            }),
            cx,
        )
    }

    /// `requestLogSessionToFile`: ask where (the backend suggests a file), then start.
    pub fn request_log_session_to_file(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        let Some(profile) = tab.profile.as_ref().and_then(SessionProfile::as_terminal) else {
            return;
        };
        let Some(backend) = self.data.backend() else {
            return;
        };
        let tab_id = tab_id.to_string();
        cx.spawn(async move |this, cx| {
            let suggested = match backend
                .suggest_session_log_file(profile, Some(&tab.title))
                .await
            {
                Ok(path) => std::path::PathBuf::from(path),
                Err(error) => {
                    let _ = this.update(cx, |this, cx| {
                        this.show_toast(Status::Error, error.message, cx)
                    });
                    return;
                }
            };
            let directory = suggested
                .parent()
                .map(std::path::Path::to_path_buf)
                .unwrap_or_default();
            let name = suggested
                .file_name()
                .map(|name| name.to_string_lossy().into_owned());
            let name = name.unwrap_or_else(|| "session.log".into());
            let chosen =
                cx.update(|cx| muxus_app::util::file_dialog::ask_save_path(&directory, &name, cx));
            let Some(file) = chosen.await.path().map(std::path::Path::to_path_buf) else {
                return;
            };
            let _ = this.update(cx, |this, cx| {
                let sent = this.send_to_session(
                    &tab_id,
                    LinkInput::message(TerminalClientMessage::SetLogging {
                        enabled: None,
                        paused: None,
                        capture_input: None,
                        log_to_file: Some(true),
                        log_file_path: Some(file.to_string_lossy().into_owned()),
                    }),
                    cx,
                );
                if !sent {
                    this.show_toast(Status::Error, "The session is no longer connected.", cx);
                }
            });
        })
        .detach();
    }

    /// The logging items both terminal menus show (`TopBar.tsx`).
    pub fn logging_menu_items(
        &mut self,
        tab: &TerminalTab,
        cx: &mut Context<Self>,
    ) -> Vec<MenuItem> {
        let Some(enabled) = tab.logging_enabled else {
            return Vec::new();
        };
        let id = tab.id.clone();
        let mut items = Vec::new();
        items.push(
            MenuItem::new(
                "logging-toggle",
                if enabled {
                    "Stop session logging"
                } else {
                    "Start session logging"
                },
            )
            .icon(if enabled {
                "StopCircleOutlined"
            } else {
                "PlayCircleOutline"
            })
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    this.menu = None;
                    this.set_logging(&id, LoggingChange::Enabled(!enabled), cx);
                    cx.notify();
                }
            })),
        );
        let file = tab.log_file_path.clone();
        items.push(
            MenuItem::new(
                "logging-file",
                if file.is_some() {
                    "Stop logging to file"
                } else {
                    "Log session to file…"
                },
            )
            .icon(if file.is_some() {
                "StopCircleOutlined"
            } else {
                "EditNoteOutlined"
            })
            .on_click(cx.listener({
                let id = id.clone();
                let stop = file.is_some();
                move |this, _, _, cx| {
                    this.menu = None;
                    if stop {
                        this.set_logging(&id, LoggingChange::StopFile, cx);
                    } else {
                        this.request_log_session_to_file(&id, cx);
                    }
                    cx.notify();
                }
            })),
        );
        if let Some(path) = file.clone() {
            items.push(
                MenuItem::new("logging-show-file", "Show log file")
                    .icon("FolderOpenOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.menu = None;
                        cx.reveal_path(std::path::Path::new(&path));
                        cx.notify();
                    })),
            );
        }
        if enabled || file.is_some() {
            let paused = tab.logging_paused;
            let capture = tab.capture_input;
            items.push(
                MenuItem::new(
                    "logging-pause",
                    if paused {
                        "Resume session logging"
                    } else {
                        "Pause session logging"
                    },
                )
                .icon(if paused {
                    "PlayCircleOutline"
                } else {
                    "PauseCircleOutline"
                })
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.menu = None;
                        this.set_logging(&id, LoggingChange::Paused(!paused), cx);
                        cx.notify();
                    }
                })),
            );
            items.push(
                MenuItem::new(
                    "logging-capture",
                    if capture {
                        "Suppress sensitive input"
                    } else {
                        "Record input (may include secrets)"
                    },
                )
                .icon(if capture {
                    "VisibilityOffOutlined"
                } else {
                    "KeyboardAltOutlined"
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.set_logging(&id, LoggingChange::CaptureInput(!capture), cx);
                    cx.notify();
                })),
            );
        }
        items
    }

    /// `requestForceReconnect`: replace one remote session's connection, asking when live.
    pub fn request_force_reconnect(
        &mut self,
        tab_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return;
        };
        if !is_remote_session_tab(&tab) {
            return;
        }
        if tab.status == TabStatus::Connected || tab.status == TabStatus::Connecting {
            self.open_confirm(
                crate::ui::confirm::ConfirmRequest {
                    title: "Force reconnect?".into(),
                    description: Some(format!(
                        "“{}” is live. This ends its session and starts a fresh connection in the same tab. Remote programs survive only when they run in tmux or screen.",
                        tab.title
                    )),
                    confirm_label: "Force reconnect".into(),
                    destructive: true,
                    checkbox: None,
                    action: crate::ui::confirm::ConfirmAction::ForceReconnect(tab.id),
                },
                window,
                cx,
            );
        } else {
            self.tabs.force_reconnect(&tab.id);
            self.tabs_changed(cx);
        }
    }

    /// The tab's context menu (`TabStrip.tsx`).
    pub fn tab_menu(&mut self, tab_id: &str, _window: &mut Window, cx: &mut Context<Self>) -> Div {
        let theme = *cx.theme();
        let Some(tab) = self.tabs.tab(tab_id).cloned() else {
            return menu::paper(&theme);
        };
        let pane_tabs: Vec<TerminalTab> = self
            .tabs
            .pane_tabs(&tab.pane_id)
            .into_iter()
            .cloned()
            .collect();
        let to_right = closable_tab_ids_to_right(pane_tabs.iter(), &tab.id);
        let can_split = pane_tabs.len() > 1;
        let has_others = self
            .tabs
            .tabs
            .iter()
            .any(|other| other.id != tab.id && !other.pinned);
        let id = tab.id.clone();
        let pinned = tab.pinned;
        let editable = tab.profile.as_ref().and_then(|profile| {
            editable_managed_host_for_profile(
                profile,
                &self.catalog.ssh.hosts,
                &self.catalog.profiles,
            )
        });
        let ssh = tab.profile.as_ref().map(SessionProfile::kind) == Some(ProfileKind::Ssh);
        let supports_sftp = ssh
            && matches!(tab.status, TabStatus::Connecting | TabStatus::Connected)
            && tab.sftp_available != Some(false);
        let force_reconnect =
            is_remote_session_tab(&tab) && (tab.status != TabStatus::Closed || ssh);
        let reconnectable = tab.profile.is_some() && tab.status == TabStatus::Closed;
        let without_terminal = tab
            .profile
            .as_ref()
            .is_some_and(|profile| !profile.is_terminal());

        let item = |key: &'static str, text: &str, glyph: &'static str| {
            MenuItem::new(key, text.to_string()).icon(glyph)
        };
        let mut paper = menu::paper(&theme)
            .child(
                item("tab-rename", "Rename tab", "DriveFileRenameOutline").on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.menu = None;
                        this.open_rename_tab(&id, window, cx);
                    }
                })),
            )
            .child(
                item("tab-edit-host", "Edit host", "EditOutlined")
                    .disabled(editable.is_none())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        if let Some(host) = editable.clone() {
                            this.open_host_editor(HostEditorTarget::Edit(host), window, cx);
                        }
                    })),
            )
            .child(
                item(
                    "tab-pin",
                    if pinned { "Unpin tab" } else { "Pin tab" },
                    "PushPinOutlined",
                )
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.tabs.set_pinned(&id, !pinned);
                        this.menu = None;
                        this.tabs_changed(cx);
                    }
                })),
            )
            .child(
                item("tab-duplicate", "Duplicate (new session)", "ContentCopy")
                    .disabled(tab.profile.is_none())
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.tabs.duplicate_tab(&id);
                            this.menu = None;
                            this.tabs_changed(cx);
                        }
                    })),
            )
            .child(
                item("tab-new-window", "Open in new window", "OpenInNewOutlined")
                    .disabled(tab.profile.is_none())
                    .on_click(cx.listener({
                        let tab = tab.clone();
                        move |this, _, _, cx| {
                            this.menu = None;
                            this.open_tab_in_new_window(&tab, false, cx);
                        }
                    })),
            )
            .child(
                item(
                    "tab-move-window",
                    "Move to new window",
                    "DriveFileMoveOutlined",
                )
                .on_click(cx.listener({
                    let tab = tab.clone();
                    move |this, _, _, cx| {
                        this.menu = None;
                        this.open_tab_in_new_window(&tab, true, cx);
                    }
                })),
            );
        if supports_sftp {
            paper = paper.child(
                item("tab-sftp", "Open SFTP file browser", "FolderOpenOutlined").on_click(
                    cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.menu = None;
                            this.show_file_browser(&id, cx);
                            this.tabs.activate(&id);
                            this.tabs_changed(cx);
                        }
                    }),
                ),
            );
        }
        paper = paper
            .child(menu_divider(&theme))
            .child(
                item(
                    "tab-split-right",
                    "Move tab to split right",
                    "VerticalSplitOutlined",
                )
                .disabled(!can_split)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.tabs.move_tab_to_new_pane(&id, PaneDirection::Right);
                        this.menu = None;
                        this.tabs_changed(cx);
                    }
                })),
            )
            .child(
                item(
                    "tab-split-down",
                    "Move tab to split down",
                    "HorizontalSplitOutlined",
                )
                .disabled(!can_split)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.tabs.move_tab_to_new_pane(&id, PaneDirection::Down);
                        this.menu = None;
                        this.tabs_changed(cx);
                    }
                })),
            );
        if reconnectable || force_reconnect {
            paper = paper.child(menu_divider(&theme));
        }
        if reconnectable {
            let reconnect = |key: &'static str,
                             text: &'static str,
                             mode: Option<ReattachMode>,
                             cx: &mut Context<Self>| {
                let id = id.clone();
                MenuItem::new(key, text)
                    .icon("Refresh")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tabs.reconnect(&[id.as_str()], mode);
                        this.menu = None;
                        this.tabs_changed(cx);
                    }))
            };
            paper = paper.child(reconnect("tab-reconnect", "Reconnect", None, cx));
            if ssh {
                paper = paper
                    .child(reconnect(
                        "tab-reconnect-tmux",
                        "Reconnect + tmux",
                        Some(ReattachMode::Tmux),
                        cx,
                    ))
                    .child(reconnect(
                        "tab-reconnect-screen",
                        "Reconnect + screen",
                        Some(ReattachMode::Screen),
                        cx,
                    ));
            }
        }
        if force_reconnect {
            paper = paper.child(
                item(
                    "tab-force-reconnect",
                    "Force reconnect (new connection)",
                    "Refresh",
                )
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, window, cx| {
                        this.menu = None;
                        this.request_force_reconnect(&id, window, cx);
                    }
                })),
            );
        }

        // The flags, and "No flag".
        let mut colors = div().flex().gap(px(4.));
        for color in TAB_FLAG_COLORS {
            let id = id.clone();
            let swatch = try_hex(color).unwrap_or(theme.palette.primary.main);
            let selected = tab.color.as_deref() == Some(color);
            let mut dot = div()
                .id(ElementId::Name(format!("flag-{color}").into()))
                .size(px(18.))
                .rounded(px(9.))
                .bg(swatch)
                .cursor_pointer()
                .flex()
                .items_center()
                .justify_center()
                .on_click(cx.listener(move |this, _, _, cx| {
                    let next = (!selected).then(|| color.to_string());
                    this.tabs.update(
                        &id,
                        TabPatch {
                            color: Some(next),
                            ..Default::default()
                        },
                    );
                    this.menu = None;
                    this.tabs_changed(cx);
                }));
            if selected {
                dot = dot.child(icon("Check", 13., theme.components.swatch_check));
            }
            colors = colors.child(dot);
        }
        let none_anchor = AnchorBounds::default();
        let mut no_flag = div()
            .id("flag-none")
            .size(px(18.))
            .rounded(px(9.))
            .border_1()
            .border_color(theme.palette.divider)
            .cursor_pointer()
            .flex()
            .items_center()
            .justify_center()
            .child(muxus_ui::widgets::bounds_probe(&none_anchor))
            .tooltip(Tooltip::anchored(
                "No flag",
                none_anchor,
                muxus_ui::widgets::Placement::Top,
            ))
            .on_click(cx.listener({
                let id = id.clone();
                move |this, _, _, cx| {
                    this.tabs.update(
                        &id,
                        TabPatch {
                            color: Some(None),
                            ..Default::default()
                        },
                    );
                    this.menu = None;
                    this.tabs_changed(cx);
                }
            }));
        if tab.color.is_none() {
            no_flag = no_flag.child(icon("Check", 13., theme.palette.text.disabled));
        }
        paper = paper.child(menu_divider(&theme)).child(
            div()
                .px(px(16.))
                .py(px(4.))
                .child(
                    div()
                        .variant(theme.typography.caption)
                        .text_color(theme.palette.text.secondary)
                        .mb(px(4.))
                        .child("Flag"),
                )
                .child(colors.child(no_flag)),
        );

        if !without_terminal {
            let selected = self
                .multi_exec
                .read(cx)
                .state()
                .selected_ids
                .iter()
                .any(|selected| selected == &id);
            paper = paper.child(menu_divider(&theme)).child(
                item(
                    "tab-multi-exec",
                    if selected {
                        "Remove from multi-execution"
                    } else {
                        "Add to multi-execution"
                    },
                    "PodcastsOutlined",
                )
                .disabled(tab.status != TabStatus::Connected)
                .on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.menu = None;
                        this.multi_exec.update(cx, |multi, cx| {
                            multi.update(cx, |state| state.toggle_target(&id))
                        });
                        cx.notify();
                    }
                })),
            );
        }
        if tab.login_sequence.is_some() {
            paper = paper.child(
                item("tab-cancel-login", "Cancel login sequence", "Close").on_click(cx.listener({
                    let id = id.clone();
                    move |this, _, _, cx| {
                        this.menu = None;
                        this.send_to_session(
                            &id,
                            LinkInput::message(TerminalClientMessage::CancelLoginSequence),
                            cx,
                        );
                        cx.notify();
                    }
                })),
            );
        }
        if !without_terminal {
            let connected = tab.status == TabStatus::Connected && tab.logging_enabled.is_some();
            let enabled = tab.logging_enabled == Some(true);
            paper = paper
                .child(
                    item(
                        "tab-logging",
                        if enabled {
                            "Stop session logging"
                        } else {
                            "Start session logging"
                        },
                        if enabled {
                            "StopCircleOutlined"
                        } else {
                            "PlayCircleOutline"
                        },
                    )
                    .disabled(!connected)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.menu = None;
                            this.set_logging(&id, LoggingChange::Enabled(!enabled), cx);
                            cx.notify();
                        }
                    })),
                )
                .child(
                    item(
                        "tab-log-file",
                        if tab.log_file_path.is_some() {
                            "Stop logging to file"
                        } else {
                            "Log session to file…"
                        },
                        if tab.log_file_path.is_some() {
                            "StopCircleOutlined"
                        } else {
                            "EditNoteOutlined"
                        },
                    )
                    .disabled(!connected)
                    .on_click(cx.listener({
                        let id = id.clone();
                        let stop = tab.log_file_path.is_some();
                        move |this, _, _, cx| {
                            this.menu = None;
                            if stop {
                                this.set_logging(&id, LoggingChange::StopFile, cx);
                            } else {
                                this.request_log_session_to_file(&id, cx);
                            }
                            cx.notify();
                        }
                    })),
                );
        }
        if let Some(kind) = tab.profile.as_ref().map(SessionProfile::kind)
            && matches!(
                kind,
                ProfileKind::Serial | ProfileKind::Telnet | ProfileKind::Ssh
            )
        {
            let attached = tab.status == TabStatus::Connected
                || (tab.status == TabStatus::Connecting && tab.conn_id.is_some());
            paper = paper.child(
                item("tab-send-break", "Send BREAK", "PowerInputOutlined")
                    .chord(muxus_app::keymap::hints::chord_label(
                        "terminal.send-break",
                        &Default::default(),
                    ))
                    .disabled(!attached)
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, _, cx| {
                            this.menu = None;
                            this.send_break(&id, cx);
                            cx.notify();
                        }
                    })),
            );
        }
        let others: Vec<String> = self
            .tabs
            .tabs
            .iter()
            .filter(|other| other.id != id && !other.pinned)
            .map(|other| other.id.clone())
            .collect();
        paper
            .child(menu_divider(&theme))
            .child(
                item("tab-close", "Close tab", "Close")
                    .chord(muxus_app::keymap::hints::chord_label(
                        "tab.close",
                        &Default::default(),
                    ))
                    .on_click(cx.listener({
                        let id = id.clone();
                        move |this, _, window, cx| {
                            this.menu = None;
                            this.request_close_tabs(vec![id.clone()], window, cx);
                        }
                    })),
            )
            .child(
                item("tab-close-others", "Close other tabs", "Close")
                    .disabled(!has_others)
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.request_close_tabs(others.clone(), window, cx);
                    })),
            )
            .child(
                item("tab-close-right", "Close tabs to the right", "Close")
                    .disabled(to_right.is_empty())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.menu = None;
                        this.request_close_tabs(to_right.clone(), window, cx);
                    })),
            )
    }

    /// `openTabInNewWindow` / `moveTabToNewWindow`: the session in a window of its own (a
    /// new session there; a move closes this tab once the new one is open).
    pub fn open_tab_in_new_window(
        &mut self,
        tab: &TerminalTab,
        moving: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(profile) = tab.profile.clone() else {
            return;
        };
        // A move takes the live session along (the new window attaches to it); without one
        // (or for a copy) the new window starts its own.
        let transfer = if moving {
            self.detach_terminal(&tab.id, cx)
        } else {
            None
        };
        crate::ui::open_main_window(
            crate::ui::MainWindowLaunch::Session {
                profile: Box::new(profile),
                title: tab.title.clone(),
                color: tab.color.clone(),
                transfer,
            },
            cx,
        );
        if moving {
            self.tabs.close(&tab.id);
            self.tabs_changed(cx);
        }
    }
}

/// Output lines shown per session (`PREVIEW_LINES`).
const PREVIEW_LINES: usize = 2;

impl AppShell {
    /// `HostSessionsMenu`: the host's open sessions, newest status and folder, and a new one.
    pub fn host_sessions_menu(
        &mut self,
        host: &muxus_app::hosts::ManagedHost,
        cx: &mut Context<Self>,
    ) -> Div {
        let theme = *cx.theme();
        let p = theme.palette;
        let key = host.key();
        let sessions: Vec<TerminalTab> =
            muxus_app::features::launcher::sessions::host_session_tabs(
                &self.tabs.root,
                &self.tabs.tabs,
                &key,
            )
            .into_iter()
            .cloned()
            .collect();
        let numbers: std::collections::HashMap<String, usize> =
            muxus_app::state::tabs::tabs_in_order(&self.tabs.root, &self.tabs.tabs)
                .into_iter()
                .enumerate()
                .map(|(index, tab)| (tab.id.clone(), index + 1))
                .collect();
        let active = self.tabs.active_id.clone();
        let header = if sessions.len() == 1 {
            "1 open session".to_string()
        } else {
            format!("{} open sessions", sessions.len())
        };
        // A dense list (`dense: true`): 11px subheader on a 28px line.
        let mut paper = menu::paper(&theme).min_w(px(260.)).max_w(px(380.)).child(
            div()
                .px(px(16.))
                .h(px(28.))
                .flex()
                .items_center()
                .text_size(px(11.))
                .font_weight(muxus_ui::gpui_kit::FontWeight(500.))
                .text_color(p.text.secondary)
                .child(header),
        );
        for tab in sessions {
            let detail = match tab.status {
                TabStatus::Connected => tab.terminal_cwd.clone(),
                TabStatus::Connecting => Some("Connecting…".to_string()),
                TabStatus::Interrupted => Some(
                    tab.failure_reason
                        .clone()
                        .unwrap_or_else(|| "Connection interrupted".into()),
                ),
                TabStatus::Closed => Some("Disconnected — select to reconnect".into()),
                TabStatus::Idle => None,
            };
            let current = active.as_deref() == Some(tab.id.as_str());
            let where_ = if current {
                "Current tab".to_string()
            } else {
                format!(
                    "Tab {}",
                    numbers
                        .get(&tab.id)
                        .map(usize::to_string)
                        .unwrap_or_default()
                )
            };
            // `recentLines(2)`: enough output to tell same-named sessions apart.
            let preview: Vec<String> = self
                .terminals
                .get(&tab.id)
                .map(|terminal| {
                    terminal
                        .view
                        .read(cx)
                        .emulator()
                        .recent_lines(PREVIEW_LINES)
                })
                .unwrap_or_default();
            let glyph = tab
                .profile
                .as_ref()
                .map(|profile| {
                    if profile.kind() == ProfileKind::Local {
                        "Terminal"
                    } else {
                        muxus_app::sidebar::kind_icon::host_kind_icon_name(profile.kind())
                    }
                })
                .unwrap_or("Terminal");
            let glyph_ink = tab
                .color
                .as_deref()
                .and_then(try_hex)
                .unwrap_or(p.text.secondary);
            let dot = match tab.status {
                TabStatus::Connected => p.success.main,
                TabStatus::Connecting | TabStatus::Interrupted => p.warning.main,
                TabStatus::Closed | TabStatus::Idle => p.text.disabled,
            };
            let badge = div()
                .relative()
                .flex()
                .flex_none()
                .mt(px(2.))
                .child(icon(glyph, 16., glyph_ink))
                .child(
                    div()
                        .absolute()
                        .right(px(-3.))
                        .bottom(px(-2.))
                        .size(px(7.))
                        .rounded_full()
                        .bg(dot)
                        .border_2()
                        .border_color(p.background_paper),
                );
            let mut body = div().flex().flex_col().min_w_0().child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .min_w_0()
                    .child(
                        div().flex_1().min_w_0().child(
                            muxus_ui::label::label(tab.title.clone())
                                .size(13.)
                                .weight(450.)
                                .color(p.text.primary),
                        ),
                    )
                    .child(
                        muxus_ui::label::label(where_)
                            .size(11.)
                            .color(p.text.secondary)
                            .no_truncate(),
                    ),
            );
            if let Some(detail) = detail {
                body = body.child(
                    muxus_ui::label::label(detail)
                        .size(11.5)
                        .color(p.text.secondary),
                );
            }
            if !preview.is_empty() {
                let mut lines = div()
                    .mt(px(4.))
                    .px(px(6.))
                    .py(px(2.))
                    .rounded(px(6.))
                    .bg(p.action.hover)
                    .flex()
                    .flex_col();
                for line in preview {
                    lines = lines.child(
                        muxus_ui::label::label(line)
                            .family(muxus_ui::theme::fonts::MONO)
                            .size(10.5)
                            .line_height(15.75)
                            .color(p.text.secondary),
                    );
                }
                body = body.child(lines);
            }
            let item = MenuItem::new(
                ElementId::Name(format!("host-session-{}", tab.id).into()),
                tab.title.clone(),
            )
            .prefix(badge, 10.)
            .body(body)
            .selected(current);
            let id = tab.id.clone();
            let closed = tab.status == TabStatus::Closed;
            paper = paper.child(item.on_click(cx.listener(move |this, _, window, cx| {
                this.menu = None;
                this.tabs.activate(&id);
                if closed {
                    this.tabs.reconnect(&[id.as_str()], None);
                }
                this.focused_tab = None;
                this.focus_terminal(&id, window, cx);
                this.tabs_changed(cx);
            })));
        }
        let host = host.clone();
        paper.child(menu_divider(&theme)).child(
            MenuItem::new("host-session-new", "New session")
                .prefix(icon("Add", 16., p.action.active), 10.)
                .body(
                    div()
                        .flex()
                        .items_center()
                        .child(
                            div().flex_1().child(
                                muxus_ui::label::label("New session")
                                    .size(13.)
                                    .color(p.text.primary),
                            ),
                        )
                        .child(
                            muxus_ui::label::label("Middle-click")
                                .size(11.)
                                .color(p.text.secondary)
                                .no_truncate(),
                        ),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.menu = None;
                    this.open_host(&host, true, None, cx);
                })),
        )
    }
}
