//! `client/src/layout/SftpWindow.tsx`: the file browser in a window of its
//! own, attached to the source tab's connection. The window holds the
//! connection open (`/ws/sftp/:connId/lease`) while it shows it; files open
//! in an editor workspace beside the browser.

use std::sync::Arc;

use muxus_ssh::sftp::SftpWindowLease;
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Context, Entity, FocusHandle, Focusable, FontWeight,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Pixels, Render, Styled,
    Subscription, Window, WindowBounds, WindowOptions, div, px, size,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, layout};
use muxus_ui::widgets::window_frame;

use super::editor::registry::request_close_remote_editor;
use super::editor::workspace::{
    EditorSource, RemoteEditorEvent, RemoteEditorOptions, RemoteEditorWorkspace,
};
use super::panel::{SftpPanel, SftpPanelEvent, SftpPanelOptions};
use super::services::{notify_error, services};

/// What an SFTP window opens on (`AppWindowLaunch` of kind `sftp`).
#[derive(Clone, Debug)]
pub struct SftpWindowLaunch {
    pub conn_id: String,
    /// The source tab's title.
    pub title: String,
    /// The folder the browser was showing.
    pub path: String,
}

/// The window's root view.
pub struct SftpWindow {
    launch: SftpWindowLaunch,
    editor_id: String,
    panel: Entity<SftpPanel>,
    workspace: Option<Entity<RemoteEditorWorkspace>>,
    paths: Vec<String>,
    active_path: Option<String>,
    focus: FocusHandle,
    _lease: Option<Arc<SftpWindowLease>>,
    _subscriptions: Vec<Subscription>,
}

impl Focusable for SftpWindow {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SftpWindow {
    pub fn new(launch: SftpWindowLaunch, window: &mut Window, cx: &mut Context<Self>) -> Self {
        window.set_window_title(&format!("{} — SFTP — Muxus", launch.title));
        let lease = match services(cx).backend.hold_sftp_window(&launch.conn_id) {
            Ok(lease) => Some(lease),
            Err(error) => {
                notify_error(&error, cx);
                None
            }
        };
        let mut options = SftpPanelOptions::new(launch.conn_id.clone());
        options.host_label = Some(launch.title.clone());
        options.initial_path = launch.path.clone();
        options.fill = true;
        let panel = cx.new(|cx| SftpPanel::new(options, window, cx));
        let subscription = cx.subscribe_in(&panel, window, |this, _, event, window, cx| {
            if let SftpPanelEvent::OpenFile(path) = event {
                this.open_file(path.clone(), window, cx);
            }
        });
        Self {
            editor_id: format!("sftp-window-{}", launch.conn_id),
            launch,
            panel,
            workspace: None,
            paths: Vec::new(),
            active_path: None,
            focus: cx.focus_handle(),
            _lease: lease,
            _subscriptions: vec![subscription],
        }
    }

    pub fn panel(&self) -> &Entity<SftpPanel> {
        &self.panel
    }

    /// `openFile`: add the file to the workspace and bring it forward.
    pub fn open_file(&mut self, path: String, window: &mut Window, cx: &mut Context<Self>) {
        if !self.paths.contains(&path) {
            self.paths.push(path.clone());
        }
        self.active_path = Some(path.clone());
        match &self.workspace {
            Some(workspace) => workspace.update(cx, |workspace, cx| {
                workspace.set_paths(self.paths.clone(), self.active_path.clone(), window, cx)
            }),
            None => {
                let mut options = RemoteEditorOptions::new(
                    self.editor_id.clone(),
                    EditorSource::Sftp(Some(self.launch.conn_id.clone())),
                );
                options.paths = self.paths.clone();
                options.active_path = self.active_path.clone();
                options.active = true;
                let workspace = cx.new(|cx| RemoteEditorWorkspace::new(options, window, cx));
                let subscription =
                    cx.subscribe_in(&workspace, window, |this, _, event, window, cx| {
                        match event {
                            RemoteEditorEvent::Activate(path) => {
                                this.active_path = Some(path.clone())
                            }
                            RemoteEditorEvent::Close(path) => this.close_file(path, window, cx),
                            RemoteEditorEvent::ShowTerminal => {}
                        }
                        cx.notify();
                    });
                self._subscriptions.push(subscription);
                self.workspace = Some(workspace);
            }
        }
        self.panel.update(cx, |panel, cx| panel.set_shown(true, cx));
        cx.notify();
    }

    /// `closeFile`: the neighbour takes the closed file's place.
    fn close_file(&mut self, path: &str, _window: &mut Window, cx: &mut Context<Self>) {
        let Some(index) = self.paths.iter().position(|p| p == path) else {
            return;
        };
        self.paths.remove(index);
        if self.active_path.as_deref() == Some(path) {
            self.active_path = self
                .paths
                .get(index.min(self.paths.len().saturating_sub(1)))
                .cloned();
        }
        if self.paths.is_empty() {
            self.workspace = None;
        }
        cx.notify();
    }

    /// The close-tab chord: the file in front first, then the window.
    fn on_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let keystroke = &event.keystroke;
        let primary = keystroke.modifiers.platform || keystroke.modifiers.control;
        if primary && !keystroke.modifiers.shift && keystroke.key == "w" {
            cx.stop_propagation();
            if !request_close_remote_editor(&self.editor_id) {
                window.remove_window();
            }
        }
    }
}

impl Render for SftpWindow {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        muxus_ui::zoom::apply_to_window(window);
        let theme = *cx.theme();
        let p = theme.palette;
        let subtitle1 = theme.typography.subtitle1;
        let caption = theme.typography.caption;
        let fill = self.workspace.is_none();
        self.panel.update(cx, |panel, cx| {
            if panel.fills() != fill {
                panel.set_fill(fill, cx);
            }
        });
        // The bar is the window's title bar (`WebkitAppRegion: 'drag'`), with the window
        // controls Electron's title bar overlay draws at its right end.
        let controls = window_frame::window_controls(window, cx);
        let bar = window_frame::title_bar(div().id("sftp-title-bar"))
            .flex()
            .flex_none()
            .items_center()
            .gap(px(10.))
            .h(px(layout::TOP_BAR_HEIGHT))
            .pl(px(window_frame::traffic_light_inset(window).unwrap_or(16.)))
            .pr(px(if controls.is_some() { 0. } else { 16. }))
            .bg(p.sidebar)
            .border_b_1()
            .border_color(p.divider)
            .child(muxus_ui::icon::logo(26., window, cx))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .min_w_0()
                    .child(
                        label(self.launch.title.clone())
                            .size(subtitle1.size)
                            .weight(700.)
                            .line_height(subtitle1.line_height)
                            .color(p.text.primary),
                    )
                    .child(
                        label("SFTP")
                            .size(caption.size)
                            .line_height(caption.line_height)
                            .color(p.text.secondary)
                            .no_truncate(),
                    ),
            )
            .child(div().flex_1())
            .when_some(controls, |bar, controls| bar.child(controls));
        let mut body = div().flex().flex_1().min_h_0().child(self.panel.clone());
        if let Some(workspace) = &self.workspace {
            body = body.child(div().flex_1().min_w_0().child(workspace.clone()));
        }
        let root = div()
            .id("sftp-window")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .size_full()
            .flex()
            .flex_col()
            .bg(p.background_default)
            .font_weight(FontWeight(450.))
            .child(bar)
            .child(body);
        window_frame::decorate_window(root, window, cx)
    }
}

/// The SFTP window's options: frameless, with its own title bar and controls.
pub fn sftp_window_options(title: &str, bounds: Bounds<Pixels>) -> WindowOptions {
    window_frame::app_window_options(
        format!("{title} — SFTP — Muxus"),
        WindowBounds::Windowed(bounds),
        size(
            muxus_ui::zoom::window_px(800.),
            muxus_ui::zoom::window_px(500.),
        ),
        crate::features::desktop::APP_ID,
    )
}

/// Open the file browser in a window of its own (`openAppWindow({ kind: 'sftp' })`).
pub fn open_sftp_window(
    launch: SftpWindowLaunch,
    cx: &mut App,
) -> Option<(AnyWindowHandle, Entity<SftpWindow>)> {
    // `createWindow` for an SFTP launch: the main window's size held to 960–1280 by
    // 640–900, the same frameless look (the app draws the frame and the controls).
    let bounds = Bounds::centered(
        None,
        size(
            muxus_ui::zoom::window_px(1280.),
            muxus_ui::zoom::window_px(900.),
        ),
        cx,
    );
    let options = sftp_window_options(&launch.title, bounds);
    muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
        cx.new(|cx| SftpWindow::new(launch, window, cx))
    })
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_ui::gpui_kit::{WindowDecorations, point};

    /// The window used the platform's default decorations, which GNOME on Wayland does not
    /// draw: it had no close button at all.
    #[test]
    fn the_sftp_window_draws_its_own_frame_and_controls() {
        let options = sftp_window_options(
            "web-01",
            Bounds::new(point(px(0.), px(0.)), size(px(1280.), px(900.))),
        );
        assert_eq!(options.window_decorations, Some(WindowDecorations::Client));
        assert_eq!(
            options.titlebar.and_then(|t| t.title).as_deref(),
            Some("web-01 — SFTP — Muxus")
        );
    }
}
