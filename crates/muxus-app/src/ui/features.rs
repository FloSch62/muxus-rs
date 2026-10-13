//! Where the feature screens meet the window: the app-wide services they share (installed
//! once at startup), the registry of main windows, and the window's openers for the feature
//! dialogs (`App.tsx`, `AppShell.tsx`).

use muxus_app::features::import::dialog::{SessionImportEvent, open_session_import};
use muxus_app::features::logs::view::{LogViewerEvent, open_log_viewer};
use muxus_app::features::settings::hooks::{
    SessionImportSource, SettingsHooks, set_settings_hooks, settings_hooks,
};
use muxus_app::features::settings::view::shortcuts::{ShortcutsDialog, ShortcutsEvent};
use muxus_app::features::settings::{
    PrefsChanged, SettingsPrefs, SettingsTarget, open_settings_window,
};
use muxus_import::review::ImportVendor;
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, Entity, Global, SharedString, WeakEntity, Window,
};
use muxus_ui::theme::Status;

use crate::ui::shell::AppShell;

/// Every main window's shell, oldest first.
#[derive(Default)]
struct Shells(Vec<WeakEntity<AppShell>>);

impl Global for Shells {}

/// Remember a main window (its shell registers itself when created).
pub fn register_shell(shell: WeakEntity<AppShell>, cx: &mut App) {
    let mut shells = cx
        .try_global::<Shells>()
        .map(|s| s.0.clone())
        .unwrap_or_default();
    shells.retain(|shell| shell.upgrade().is_some());
    shells.push(shell);
    cx.set_global(Shells(shells));
}

/// The shells of open main windows.
pub fn shells(cx: &App) -> Vec<Entity<AppShell>> {
    cx.try_global::<Shells>()
        .map(|shells| shells.0.iter().filter_map(WeakEntity::upgrade).collect())
        .unwrap_or_default()
}

/// The main window the user works in: the focused one, else the one opened first.
pub fn active_shell(cx: &App) -> Option<Entity<AppShell>> {
    let shells = shells(cx);
    let active = cx.active_window();
    shells
        .iter()
        .find(|shell| active.is_some_and(|window| shell.read(cx).window_handle == Some(window)))
        .or_else(|| shells.first())
        .cloned()
}

/// The shell of the main window `window` is (commands run in the window they came from).
pub fn shell_for_window(window: &Window, cx: &App) -> Option<Entity<AppShell>> {
    let handle = window.window_handle();
    shells(cx)
        .into_iter()
        .find(|shell| shell.read(cx).window_handle == Some(handle))
        .or_else(|| active_shell(cx))
}

/// The middle of the window (`menuCenter`).
pub fn window_center(window: &Window) -> muxus_ui::gpui_kit::Point<muxus_ui::gpui_kit::Pixels> {
    let size = window.viewport_size();
    muxus_ui::gpui_kit::point(size.width / 2., size.height / 2.)
}

/// Bring a main window forward (`win.show(); win.focus()`).
pub fn focus_shell_window(shell: &Entity<AppShell>, cx: &mut App) {
    if let Some(handle) = shell.read(cx).window_handle {
        let _ = handle.update(cx, |_, window, _| window.activate_window());
    }
}

/// A notification in the main window the user works in (`showToast`).
pub fn toast(status: Status, message: SharedString, cx: &mut App) {
    match active_shell(cx) {
        Some(shell) => shell.update(cx, |shell, cx| shell.show_toast(status, message, cx)),
        None => eprintln!("{message}"),
    }
}

/// Run `f` on the active main window's shell, inside its window.
pub fn with_active_shell(
    cx: &mut App,
    f: impl FnOnce(&mut AppShell, &mut Window, &mut Context<AppShell>) + 'static,
) {
    let Some(shell) = active_shell(cx) else {
        return;
    };
    let Some(handle) = shell.read(cx).window_handle else {
        return;
    };
    let _ = handle.update(cx, |_, window, cx| {
        shell.update(cx, |shell, cx| f(shell, window, cx));
    });
}

/// The services every feature screen shares, installed once after `muxus_ui::init` and
/// `SettingsPrefs::init`.
pub fn install(backend: &Backend, cx: &mut App) {
    muxus_app::features::launcher::kit::set_toast_hook(toast, cx);
    // The interface scale is one preference for every window (`applyInterfaceZoom`).
    if let Some(shared) = SettingsPrefs::global(cx) {
        let zoom = shared.read(cx).prefs().interface_zoom;
        muxus_ui::zoom::set_zoom(muxus_store::prefs::layout::clamp_interface_zoom(zoom) as f32);
        cx.subscribe(&shared, |_, event: &PrefsChanged, cx| {
            let zoom =
                muxus_store::prefs::layout::clamp_interface_zoom(event.current.interface_zoom);
            if muxus_ui::zoom::set_zoom(zoom as f32) {
                cx.refresh_windows();
            }
        })
        .detach();
    }
    muxus_app::features::sftp::services::init(
        backend.clone(),
        std::rc::Rc::new(|status, message, details, cx| {
            let message = match details {
                Some(details) => SharedString::from(format!("{message} {details}")),
                None => message,
            };
            toast(status, message, cx);
        }),
        cx,
    );
    muxus_app::features::remote_desktop::init(cx);
    let mut hooks: SettingsHooks = settings_hooks(cx);
    hooks.open_log_viewer = Some(std::rc::Rc::new(|_, cx| {
        with_active_shell(cx, |shell, window, cx| shell.open_log_viewer(window, cx));
    }));
    hooks.import_sessions = Some(std::rc::Rc::new(|source, _, cx| {
        let vendor = match source {
            SessionImportSource::MobaXterm => ImportVendor::MobaXterm,
            SessionImportSource::SecureCrt => ImportVendor::SecureCrt,
        };
        with_active_shell(cx, move |shell, window, cx| {
            shell.open_session_import(vendor, window, cx)
        });
    }));
    set_settings_hooks(hooks, cx);
    // "New secret…" in host editor fields opens the vault dialog over the active window.
    muxus_app::features::vault::secret_dialog::set_new_secret_hook(
        |on_created, cx| {
            with_active_shell(cx, move |shell, window, cx| {
                shell.open_new_secret(on_created, window, cx)
            });
        },
        cx,
    );
}

impl AppShell {
    /// The vault's secret dialog for a field that asked for a new secret.
    pub fn open_new_secret(
        &mut self,
        on_created: muxus_app::features::vault::secret_dialog::SecretCreated,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        use muxus_app::features::vault::secret_dialog::{
            VaultSecretEvent, open_vault_secret_dialog,
        };
        let Some(backend) = self.data.backend() else {
            return;
        };
        let dialog = open_vault_secret_dialog(backend, None, "", window, cx);
        let on_created = std::cell::RefCell::new(Some(on_created));
        self.mount_overlay(
            "vault-secret",
            dialog,
            window,
            cx,
            move |this, _, event: &VaultSecretEvent, window, cx| {
                if let VaultSecretEvent::Saved(result) = event
                    && let Some(callback) = on_created.borrow_mut().take()
                {
                    callback((**result).clone(), cx);
                }
                this.close_overlay("vault-secret", window, cx);
            },
        );
    }

    /// `setSettingsOpen(true)`: the settings window, at `target` when given.
    pub fn open_settings(&mut self, target: Option<SettingsTarget>, cx: &mut Context<Self>) {
        if open_settings_window(target, cx).is_none() {
            self.show_toast(
                Status::Info,
                "Settings need the Muxus backend, which is not running.",
                cx,
            );
        }
    }

    /// `setShortcutsOpen(true)`: the shortcuts editor over this window.
    pub fn open_shortcuts(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(prefs) = SettingsPrefs::global(cx) else {
            self.open_settings(None, cx);
            return;
        };
        let dialog = cx.new(|cx| ShortcutsDialog::new(prefs, window, cx));
        self.mount_overlay(
            "shortcuts",
            dialog,
            window,
            cx,
            |this, _, event: &ShortcutsEvent, window, cx| match event {
                ShortcutsEvent::Close => this.close_overlay("shortcuts", window, cx),
            },
        );
    }

    pub fn open_log_viewer(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let dialog = open_log_viewer(backend, window, cx);
        self.mount_overlay(
            "log-viewer",
            dialog,
            window,
            cx,
            |this, _, event: &LogViewerEvent, window, cx| match event {
                LogViewerEvent::Close => this.close_overlay("log-viewer", window, cx),
            },
        );
    }

    pub fn open_session_import(
        &mut self,
        vendor: ImportVendor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.data.backend() else {
            return;
        };
        let dialog = open_session_import(backend, vendor, window, cx);
        self.mount_overlay(
            "session-import",
            dialog,
            window,
            cx,
            |this, _, event: &SessionImportEvent, window, cx| match event {
                SessionImportEvent::Close => this.close_overlay("session-import", window, cx),
                SessionImportEvent::Imported(_) => {
                    this.load_catalog(cx);
                    this.close_overlay("session-import", window, cx);
                }
            },
        );
    }
}
