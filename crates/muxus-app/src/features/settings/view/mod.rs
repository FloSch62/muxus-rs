//! The GPUI views of the settings: the window, its sections and the
//! keyboard shortcuts editor.
//!
//! Hook points for the rest of the app:
//! - [`SettingsPrefs::init`] once at startup with the backend and its prefs;
//!   subscribe to the entity for [`PrefsChanged`] to apply changes live.
//! - [`open_settings_window`] from the gear button, menus and deep links
//!   (`SettingsTarget` names a section and an entry in it).
//! - [`ShortcutsDialog`] for the top bar's keyboard button: any window can
//!   mount it (see [`open_shortcuts`] for the settings window's own).
//!
//! [`SettingsPrefs::init`]: super::prefs_model::SettingsPrefs::init
//! [`PrefsChanged`]: super::prefs_model::PrefsChanged

pub mod about;
pub mod appearance;
pub mod behavior;
pub mod color;
pub mod common;
pub mod custom_schemes;
pub mod data;
pub mod debug;
pub mod fields;
pub mod highlighting;
pub mod io;
pub mod keyboard;
pub mod kit;
pub mod layout_preview;
pub mod local_shells;
pub mod logging;
pub mod passwords;
pub mod scheme;
pub mod shortcuts;
pub mod state;
pub mod terminal;
pub mod window;
pub mod x11;

use muxus_store::prefs::window_state::{
    Rectangle, SETTINGS_WINDOW_SIZE, WindowBounds as PlacedBounds, WindowPlacement,
    load_window_placement, save_window_placement, window_bounds,
};
use muxus_ui::gpui_kit::{
    AnyWindowHandle, App, AppContext, Bounds, Entity, Global, WeakEntity, WindowBounds, point, size,
};

use super::prefs_model::SettingsPrefs;
use super::section::SettingsTarget;
pub use shortcuts::ShortcutsDialog;
pub use window::SettingsWindow;

/// The one settings window, while it is open.
#[derive(Default)]
struct OpenSettingsWindow(Option<(AnyWindowHandle, WeakEntity<SettingsWindow>)>);

impl Global for OpenSettingsWindow {}

fn rect(bounds: Bounds<muxus_ui::gpui_kit::Pixels>) -> Rectangle {
    Rectangle {
        x: f64::from(f32::from(bounds.origin.x)),
        y: f64::from(f32::from(bounds.origin.y)),
        width: f64::from(f32::from(bounds.size.width)),
        height: f64::from(f32::from(bounds.size.height)),
    }
}

fn gpui_bounds(rect: Rectangle) -> Bounds<muxus_ui::gpui_kit::Pixels> {
    Bounds {
        origin: point(
            muxus_ui::zoom::window_px(rect.x as f32),
            muxus_ui::zoom::window_px(rect.y as f32),
        ),
        size: size(
            muxus_ui::zoom::window_px(rect.width as f32),
            muxus_ui::zoom::window_px(rect.height as f32),
        ),
    }
}

/// Bring up the settings window (`showSettingsWindow`): focus the open one
/// and show `target` in it, or open it where it was last left (on the
/// display it overlaps most), else centred over the active window.
///
/// Needs [`SettingsPrefs::init`] to have run; returns `None` otherwise.
pub fn open_settings_window(
    target: Option<SettingsTarget>,
    cx: &mut App,
) -> Option<(AnyWindowHandle, Entity<SettingsWindow>)> {
    let prefs = SettingsPrefs::global(cx)?;
    if let Some((handle, view)) = cx
        .try_global::<OpenSettingsWindow>()
        .and_then(|open| open.0.clone())
        && let Some(view) = view.upgrade()
    {
        let shown = handle.update(cx, |_, window, cx| {
            view.update(cx, |view, cx| {
                if let Some(target) = target.clone() {
                    view.show(target, window, cx);
                }
            });
            window.activate_window();
        });
        if shown.is_ok() {
            return Some((handle, view));
        }
    }
    let path = prefs
        .read(cx)
        .backend()
        .paths()
        .settings_window_state_file();
    let saved = load_window_placement(&path);
    let owner = cx
        .active_window()
        .and_then(|window| window.update(cx, |_, window, _| window.bounds()).ok())
        .map(rect);
    let areas: Vec<Rectangle> = cx
        .displays()
        .iter()
        .map(|display| rect(display.bounds()))
        .collect();
    let placed = window_bounds(saved.map(|p| p.bounds), owner, &areas, SETTINGS_WINDOW_SIZE);
    let bounds = match placed {
        PlacedBounds::Placed(rect) => gpui_bounds(rect),
        PlacedBounds::SizeOnly(size_only) => Bounds::centered(
            None,
            size(
                muxus_ui::zoom::window_px(size_only.width as f32),
                muxus_ui::zoom::window_px(size_only.height as f32),
            ),
            cx,
        ),
    };
    let maximized = saved.is_some_and(|placement| placement.maximized);
    let options = muxus_ui::widgets::window_frame::app_window_options(
        "Settings \u{2014} Muxus",
        if maximized {
            WindowBounds::Maximized(bounds)
        } else {
            WindowBounds::Windowed(bounds)
        },
        size(
            muxus_ui::zoom::window_px(560.),
            muxus_ui::zoom::window_px(420.),
        ),
        crate::features::desktop::APP_ID,
    );
    let handle = muxus_ui::gpui_kit::open_window(options, cx, move |window, cx| {
        let view = cx.new(|cx| SettingsWindow::new(prefs, target, window, cx));
        let weak = view.downgrade();
        // Unsaved logging edits hold the window, whichever way it is closed.
        muxus_ui::widgets::window_frame::on_close_request(window, cx, move |window, cx| {
            let Some(view) = weak.upgrade() else {
                return true;
            };
            let holds = view.read(cx).nav.holds_unsaved_logging();
            if holds {
                let weak = view.downgrade();
                window.defer(cx, move |window, cx| {
                    if let Some(view) = weak.upgrade() {
                        view.update(cx, |view, cx| view.ask_discard_on_close(window, cx));
                    }
                });
                return false;
            }
            save_placement(window, cx);
            true
        });
        view
    })
    .ok()?;
    cx.set_global(OpenSettingsWindow(Some((handle.0, handle.1.downgrade()))));
    Some(handle)
}

/// Remember where the settings window is (`saveSettingsWindowPlacement`).
pub fn save_placement(window: &mut muxus_ui::gpui_kit::Window, cx: &mut App) {
    let Some(prefs) = SettingsPrefs::global(cx) else {
        return;
    };
    let path = prefs
        .read(cx)
        .backend()
        .paths()
        .settings_window_state_file();
    let (bounds, maximized) = match window.window_bounds() {
        WindowBounds::Windowed(bounds) => (bounds, false),
        WindowBounds::Maximized(bounds) | WindowBounds::Fullscreen(bounds) => (bounds, true),
    };
    // Placement is a nicety; never block closing on it.
    let _ = save_window_placement(
        &path,
        &WindowPlacement {
            bounds: rect(bounds),
            maximized,
        },
    );
}

/// Open the keyboard shortcuts editor over the settings window (opening the
/// window at the Keyboard section first when needed).
pub fn open_shortcuts(cx: &mut App) {
    let target = SettingsTarget::section(super::section::SettingsSection::Keyboard);
    if let Some((handle, view)) = open_settings_window(Some(target), cx) {
        let _ = handle.update(cx, |_, window, cx| {
            view.update(cx, |view, cx| view.open_shortcuts(window, cx));
        });
    }
}
