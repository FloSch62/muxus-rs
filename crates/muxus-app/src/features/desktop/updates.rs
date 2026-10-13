//! The app side of in-app updates (`createDesktopUpdater` and the `muxus:update:*` handlers of
//! `electron/src/main.ts`, `DesktopUpdateControls.tsx`): one [`AppUpdater`] for the whole app,
//! its state as a GPUI entity every window reads, the hooks the About page and the update
//! notice use, background checks that follow the "Notify me" preference, and installing once
//! the app quits.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use muxus_core::api::{DesktopUpdateState, DesktopUpdateStatus};
use muxus_core::platform::Platform;
use muxus_session::Backend;
use muxus_session::updater::store::{
    DistributionMetadata, running_from_windows_store, store_page_url,
};
use muxus_session::updater::{
    AppUpdater, HyperHttp, ProcessUpdaterOptions, ReleaseFeed, default_cache_dir,
    install::relaunch_current,
};
use muxus_ui::gpui_kit::{App, AppContext, Entity, Global, Window};
use muxus_ui::theme::Status;

use super::main_log::{MainLogLevel, main_log};
use crate::features::launcher::kit::show_toast;
use crate::features::settings::hooks::{DesktopUpdateHooks, settings_hooks};
use crate::features::settings::{PrefsChanged, SettingsPrefs, set_settings_hooks};
use crate::features::sftp::dialogs::ConfirmOptions;
use crate::features::sftp::local_open::ask;

/// Points the updater at another release folder (a mirror or a loopback server) holding the
/// `latest*.yml` feeds and installers. Development aid; the default is [`ReleaseFeed::muxus`].
pub const FEED_OVERRIDE_VAR: &str = "MUXUS_UPDATE_FEED";

/// What the app's updater reports to the GPUI side.
enum UpdateMessage {
    State(DesktopUpdateState),
    /// An accepted install: quit, then install from the quit handler.
    Quit,
}

/// The app's updater and its latest state. Read it from any window with
/// [`DesktopUpdates::global`]; it notifies on every change.
pub struct DesktopUpdates {
    updater: AppUpdater,
    state: DesktopUpdateState,
    metadata: DistributionMetadata,
}

struct GlobalUpdates(Entity<DesktopUpdates>);

impl Global for GlobalUpdates {}

/// Options for [`init_desktop_updates`].
#[derive(Clone, Default)]
pub struct DesktopUpdateInit {
    /// Store packages: persist window placement and other state before Windows may close
    /// the app to install (`flushClientState`, `saveWindowState`).
    pub before_store_install: Option<Arc<dyn Fn() -> Result<(), String> + Send + Sync>>,
}

impl DesktopUpdates {
    pub fn global(cx: &App) -> Option<Entity<DesktopUpdates>> {
        cx.try_global::<GlobalUpdates>()
            .map(|global| global.0.clone())
    }

    pub fn state(&self) -> &DesktopUpdateState {
        &self.state
    }

    pub fn updater(&self) -> &AppUpdater {
        &self.updater
    }
}

fn process_env() -> HashMap<String, String> {
    std::env::vars().collect()
}

fn home_dir(env: &HashMap<String, String>) -> PathBuf {
    env.get("HOME")
        .or_else(|| env.get("USERPROFILE"))
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
}

/// Create the app's updater (once, at startup, after [`SettingsPrefs::init`]), install the
/// update hooks into the settings hooks and follow `notifyOnNewVersion`.
pub fn init_desktop_updates(
    backend: &Backend,
    init: DesktopUpdateInit,
    cx: &mut App,
) -> Entity<DesktopUpdates> {
    let (sender, receiver) = async_channel::unbounded::<UpdateMessage>();
    let env = process_env();
    let platform = Platform::current();
    let version = env!("CARGO_PKG_VERSION").to_string();
    let feed = env
        .get(FEED_OVERRIDE_VAR)
        .filter(|url| !url.is_empty())
        .map(|base_url| ReleaseFeed::Generic {
            base_url: base_url.clone(),
        })
        .unwrap_or_else(ReleaseFeed::muxus);
    let metadata = DistributionMetadata::built_in();
    let broadcast = sender.clone();
    let quit = sender;
    let relaunch_env = env.clone();
    let updater = AppUpdater::for_process(ProcessUpdaterOptions {
        version: version.clone(),
        packaged: !cfg!(debug_assertions),
        platform,
        cache_dir: default_cache_dir(platform, &env, &home_dir(&env)),
        env,
        metadata: metadata.clone(),
        windows_store: running_from_windows_store(),
        feed,
        http: Arc::new(HyperHttp::new(&version)),
        broadcast: Arc::new(move |state| {
            let _ = broadcast.try_send(UpdateMessage::State(state.clone()));
        }),
        prepare_install: Arc::new(move || {
            let _ = quit.try_send(UpdateMessage::Quit);
        }),
        // The backend has already shut down; reopen the installed version so the user is not
        // left without Muxus.
        recover_install: Arc::new(move || {
            if let Err(error) = relaunch_current(&relaunch_env) {
                main_log(
                    MainLogLevel::Error,
                    "could not reopen Muxus after a failed update",
                    Some(&error),
                );
            }
        }),
        before_store_install: init
            .before_store_install
            .unwrap_or_else(|| Arc::new(|| Ok(()))),
        store_owner_window: Arc::new(|| Ok("0".into())),
        runtime: backend.runtime().clone(),
    });
    let entity = cx.new(|_| DesktopUpdates {
        state: updater.state(),
        updater: updater.clone(),
        metadata,
    });
    cx.set_global(GlobalUpdates(entity.clone()));

    let updates = entity.clone();
    cx.spawn(async move |cx| {
        while let Ok(message) = receiver.recv().await {
            cx.update(|cx| match message {
                UpdateMessage::State(state) => {
                    updates.update(cx, |this, cx| {
                        this.state = state;
                        cx.notify();
                    });
                    // About and the update notice read the state through the settings hooks.
                    cx.refresh_windows();
                }
                UpdateMessage::Quit => cx.quit(),
            });
        }
    })
    .detach();

    // An accepted update installs only after the windows have closed; the new version waits
    // for this process to exit before it starts (see `RELAUNCH_AFTER_PID_VAR`).
    let quitting = updater.clone();
    cx.on_app_quit(move |_| {
        quitting.stop();
        if quitting.state().status == DesktopUpdateStatus::Installing {
            quitting.finish_install();
        }
        async {}
    })
    .detach();

    follow_notify_preference(&updater, cx);
    let mut hooks = settings_hooks(cx);
    hooks.desktop_update = Some(desktop_update_hooks());
    set_settings_hooks(hooks, cx);
    entity
}

/// Background checks run only while the update notifications are on.
fn follow_notify_preference(updater: &AppUpdater, cx: &mut App) {
    let Some(prefs) = SettingsPrefs::global(cx) else {
        return;
    };
    updater.set_automatic_checks(prefs.read(cx).prefs().notify_on_new_version);
    let updater = updater.clone();
    cx.subscribe(&prefs, move |_, event: &PrefsChanged, _| {
        if event.changed(|prefs| prefs.notify_on_new_version) {
            updater.set_automatic_checks(event.current.notify_on_new_version);
        }
    })
    .detach();
}

fn updater(cx: &App) -> Option<AppUpdater> {
    DesktopUpdates::global(cx).map(|entity| entity.read(cx).updater.clone())
}

/// The hooks About and the update notice call (`window.muxusDesktop` update methods).
pub fn desktop_update_hooks() -> DesktopUpdateHooks {
    DesktopUpdateHooks {
        state: std::rc::Rc::new(|cx: &App| {
            DesktopUpdates::global(cx).map(|entity| entity.read(cx).state.clone())
        }),
        check: std::rc::Rc::new(|_: &mut Window, cx: &mut App| check_for_updates(cx)),
        download: std::rc::Rc::new(|window: &mut Window, cx: &mut App| {
            // The update notice offers a Store update through its download action.
            match updater(cx) {
                Some(AppUpdater::Store(_)) => install_store_update(window, cx),
                _ => download_update(cx),
            }
        }),
        install: std::rc::Rc::new(|window: &mut Window, cx: &mut App| match updater(cx) {
            Some(AppUpdater::Store(_)) => install_store_update(window, cx),
            _ => restart_to_update(window, cx),
        }),
        open_store: std::rc::Rc::new(|_: &mut Window, cx: &mut App| open_store(cx)),
    }
}

/// `checkForDesktopUpdates`.
pub fn check_for_updates(cx: &mut App) {
    if let Some(updater) = updater(cx) {
        // The work runs on the backend runtime; the state arrives through the broadcast.
        drop(updater.check());
    }
}

/// `downloadDesktopUpdate`.
pub fn download_update(cx: &mut App) {
    match updater(cx).and_then(|updater| updater.download()) {
        Some(download) => drop(download),
        None => show_toast(
            Status::Error,
            "The download could not be started. Try again from Settings → About.",
            cx,
        ),
    }
}

/// `restartToUpdate`: confirm, then quit and install.
pub fn restart_to_update(_window: &mut Window, cx: &mut App) {
    let answer = ask(
        ConfirmOptions {
            title: "Restart Muxus to update?".into(),
            description: Some("All Muxus windows will close and every session disconnects: terminals, file transfers, tunnels and remote desktops. Save open remote files first.".into()),
            confirm_label: "Restart to update".into(),
            ..ConfirmOptions::default()
        },
        cx,
    );
    cx.spawn(async move |cx| {
        if !answer.await.confirmed {
            return;
        }
        cx.update(|cx| {
            let accepted = updater(cx).is_some_and(|updater| updater.request_install());
            if !accepted {
                show_toast(
                    Status::Error,
                    "The update could not be started. Try again from Settings → About.",
                    cx,
                );
            }
        });
    })
    .detach();
}

/// `installStoreUpdate`.
pub fn install_store_update(_window: &mut Window, cx: &mut App) {
    let answer = ask(
        ConfirmOptions {
            title: "Update Muxus through Microsoft Store?".into(),
            description: Some("Windows may close all Muxus windows to install the update, and every session disconnects. Save open remote files first. Windows asks you to confirm the download.".into()),
            confirm_label: "Update now".into(),
            ..ConfirmOptions::default()
        },
        cx,
    );
    cx.spawn(async move |cx| {
        if !answer.await.confirmed {
            return;
        }
        cx.update(|cx| {
            let accepted = updater(cx).is_some_and(|updater| updater.request_install());
            if !accepted {
                show_toast(
                    Status::Warning,
                    "The update could not be started. Check for updates again in Settings → About.",
                    cx,
                );
            }
        });
    })
    .detach();
}

/// `openMicrosoftStore`.
pub fn open_store(cx: &mut App) {
    let Some(entity) = DesktopUpdates::global(cx) else {
        return;
    };
    let url = store_page_url(&entity.read(cx).metadata);
    cx.open_url(&url);
}
