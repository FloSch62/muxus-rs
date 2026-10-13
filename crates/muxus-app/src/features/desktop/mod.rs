//! The desktop shell around the windows (what `electron/src/main.ts` did besides windows and
//! tabs): one running instance per data folder and the launches later processes hand over,
//! the in-app updater, the Links settings (system handler for ssh:// and telnet://), and the
//! shell's own log file.
//!
//! # Wiring it into the app
//!
//! 1. First thing in `main`, before the backend starts:
//!    [`main_log::init_main_log`] with the data folder, [`main_log::install_crash_capture`],
//!    then [`single_instance::acquire`] with the data folder and `std::env::args()`. On
//!    [`single_instance::Acquired::Secondary`] exit at once: the launch went to the running
//!    instance. Keep the [`single_instance::PrimaryInstance`] alive for the whole run.
//! 2. The launch this process was started with: [`events::initial_launch`].
//! 3. Later launches: `primary.events()` ([`events::DesktopEvents`]) yields
//!    [`events::DesktopEvent::Launch`] (show the focused or last used window, or open one,
//!    then run [`launch::plan_launch`] and perform the [`launch::PlannedAction`]) and
//!    [`events::DesktopEvent::Focus`] (restore, show and focus the main window). On macOS,
//!    feed `open-url` through `primary.sender()` with [`events::DesktopEvent::from_open_url`].
//! 4. After `SettingsPrefs::init`: [`updates::init_desktop_updates`]. It installs the
//!    `desktop_update` settings hook (About's update row and the update notice read it),
//!    follows `notifyOnNewVersion` for background checks, quits on an accepted install and
//!    installs from its own `on_app_quit` handler.
//! 5. Windows set the Wayland app id / X11 class to [`APP_ID`], so the desktop file and its
//!    icon belong to them.

pub mod events;
pub mod launch;
pub mod links_view;
pub mod main_log;
pub mod single_instance;
pub mod updates;

/// The desktop file name (`muxus.desktop`) and window class every Muxus window uses.
pub const APP_ID: &str = "muxus";
