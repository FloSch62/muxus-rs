//! Where Muxus keeps its files.
//!
//! The desktop app (Electron in the TypeScript build, this app in Rust) keeps
//! everything in its *user data* directory: `muxus.sqlite3`,
//! `client-state.json` (preferences), the window placement files, and in
//! development builds the `history/` root. Electron derives that directory
//! from the platform's per-user application data folder plus the app name
//! `Muxus`; a development build appends `-development` so source runs never
//! touch the installed app's data.
//!
//! The standalone TypeScript server (`node server/dist/index.js`) uses a
//! different default (`$XDG_DATA_HOME/muxus/muxus.sqlite3` on Linux); it is
//! exposed as [`server_default_database_path`] for tools that open that
//! database instead.
//!
//! Every function takes an [`Environment`] so tests (and sandboxes) never
//! read the real home directory.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The application name Electron derives the user data folder from.
pub const APP_NAME: &str = "Muxus";
/// The application database file name inside the user data folder.
pub const DATABASE_FILENAME: &str = "muxus.sqlite3";
/// Preferences and other renderer state (`Record<string, string>` as JSON).
pub const CLIENT_STATE_FILENAME: &str = "client-state.json";
/// Main window bounds.
pub const WINDOW_STATE_FILENAME: &str = "window-state.json";
/// Settings window placement.
pub const SETTINGS_WINDOW_STATE_FILENAME: &str = "settings-window-state.json";

/// The operating systems with distinct path rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Platform {
    Linux,
    MacOs,
    Windows,
}

impl Platform {
    /// The platform this build runs on (other Unixes follow the Linux rules).
    pub fn current() -> Self {
        if cfg!(target_os = "macos") {
            Platform::MacOs
        } else if cfg!(windows) {
            Platform::Windows
        } else {
            Platform::Linux
        }
    }
}

/// The parts of the process environment path rules read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Environment {
    pub platform: Platform,
    /// The user's home directory (`os.homedir()`).
    pub home: PathBuf,
    vars: HashMap<String, OsString>,
}

impl Environment {
    /// The real process environment.
    pub fn from_process() -> Self {
        let vars: HashMap<String, OsString> = std::env::vars_os()
            .filter_map(|(key, value)| key.into_string().ok().map(|key| (key, value)))
            .collect();
        let home_var = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
        let home = vars
            .get(home_var)
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(std::env::home_dir)
            .unwrap_or_default();
        Self {
            platform: Platform::current(),
            home,
            vars,
        }
    }

    /// A synthetic environment (tests, sandboxes).
    pub fn new(platform: Platform, home: impl Into<PathBuf>) -> Self {
        Self {
            platform,
            home: home.into(),
            vars: HashMap::new(),
        }
    }

    /// The same environment with one variable set.
    pub fn with_var(mut self, key: &str, value: impl Into<OsString>) -> Self {
        self.vars.insert(key.to_owned(), value.into());
        self
    }

    /// A variable's value; empty values count as unset, like `process.env.X || …`.
    pub fn var(&self, key: &str) -> Option<&OsString> {
        self.vars.get(key).filter(|value| !value.is_empty())
    }
}

/// The per-user application data folder Electron's `app.getPath('appData')`
/// returns: `$XDG_CONFIG_HOME` or `~/.config` on Linux, `~/Library/Application
/// Support` on macOS, `%APPDATA%` on Windows.
pub fn app_data_dir(env: &Environment) -> PathBuf {
    match env.platform {
        Platform::Linux => env
            .var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env.home.join(".config")),
        Platform::MacOs => env.home.join("Library").join("Application Support"),
        Platform::Windows => env
            .var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming")),
    }
}

/// The desktop app's data folder and the files in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    /// Electron's `userData`: `<appData>/Muxus`, or `<appData>/Muxus-development`
    /// for a development build.
    pub user_data: PathBuf,
    /// Whether this is a development build's data folder.
    pub development: bool,
}

impl AppPaths {
    /// The installed desktop app's data folder.
    pub fn installed(env: &Environment) -> Self {
        Self {
            user_data: app_data_dir(env).join(APP_NAME),
            development: false,
        }
    }

    /// A development build's data folder, isolated from the installed one
    /// (`developmentUserDataPath` in the TypeScript).
    pub fn development(env: &Environment) -> Self {
        let installed = Self::installed(env).user_data;
        Self {
            user_data: development_user_data_path(&installed),
            development: true,
        }
    }

    /// A data folder at an explicit location (tests, sandboxes).
    pub fn at(user_data: impl Into<PathBuf>) -> Self {
        Self {
            user_data: user_data.into(),
            development: false,
        }
    }

    /// `muxus.sqlite3`.
    pub fn database(&self) -> PathBuf {
        self.user_data.join(DATABASE_FILENAME)
    }

    /// `client-state.json`: preferences and other UI state.
    pub fn client_state_file(&self) -> PathBuf {
        self.user_data.join(CLIENT_STATE_FILENAME)
    }

    pub fn window_state_file(&self) -> PathBuf {
        self.user_data.join(WINDOW_STATE_FILENAME)
    }

    pub fn settings_window_state_file(&self) -> PathBuf {
        self.user_data.join(SETTINGS_WINDOW_STATE_FILENAME)
    }

    /// The session history root the desktop app forces, if any: development
    /// builds keep history beside their own database; installed builds leave
    /// it to the history settings (see [`history_root`]).
    pub fn forced_history_root(&self) -> Option<PathBuf> {
        self.development.then(|| self.user_data.join("history"))
    }
}

/// `${installed}-development`.
pub fn development_user_data_path(installed: &Path) -> PathBuf {
    let mut name = installed.as_os_str().to_owned();
    name.push("-development");
    PathBuf::from(name)
}

/// The standalone server's default database location (`defaultDatabasePath`).
pub fn server_default_database_path(env: &Environment) -> PathBuf {
    match env.platform {
        Platform::MacOs => env
            .home
            .join("Library")
            .join("Application Support")
            .join(APP_NAME)
            .join(DATABASE_FILENAME),
        Platform::Windows => env
            .var("APPDATA")
            .map(PathBuf::from)
            .unwrap_or_else(|| env.home.join("AppData").join("Roaming"))
            .join(APP_NAME)
            .join(DATABASE_FILENAME),
        Platform::Linux => env
            .var("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| env.home.join(".local").join("share"))
            .join("muxus")
            .join(DATABASE_FILENAME),
    }
}

/// The standalone server's history override (`--history-path` or `MUXUS_HISTORY_PATH`).
pub fn server_history_override(env: &Environment) -> Option<PathBuf> {
    env.var("MUXUS_HISTORY_PATH").map(PathBuf::from)
}

/// The default session history root beside a database (`defaultHistoryRoot`).
/// `None` for an in-memory database, which gets a temporary root.
pub fn default_history_root(database: &Path) -> Option<PathBuf> {
    if database.as_os_str() == ":memory:" {
        return None;
    }
    Some(
        database
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join("history"),
    )
}

/// The history root in effect: a forced root (development build, server
/// override), else the configured storage location, else the default beside
/// the database. Mirrors `buildApp` in the TypeScript.
pub fn history_root(
    forced: Option<&Path>,
    configured: Option<&str>,
    database: &Path,
) -> Option<PathBuf> {
    forced
        .map(Path::to_path_buf)
        .or_else(|| {
            configured
                .filter(|value| !value.is_empty())
                .map(PathBuf::from)
        })
        .or_else(|| default_history_root(database))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn desktop_data_follows_electron_user_data() {
        let linux = Environment::new(Platform::Linux, "/home/u");
        assert_eq!(
            AppPaths::installed(&linux).database(),
            PathBuf::from("/home/u/.config/Muxus/muxus.sqlite3")
        );
        let xdg = linux.clone().with_var("XDG_CONFIG_HOME", "/x/cfg");
        assert_eq!(
            AppPaths::installed(&xdg).client_state_file(),
            PathBuf::from("/x/cfg/Muxus/client-state.json")
        );
        let empty = linux.clone().with_var("XDG_CONFIG_HOME", "");
        assert_eq!(
            AppPaths::installed(&empty).user_data,
            PathBuf::from("/home/u/.config/Muxus")
        );
        let dev = AppPaths::development(&linux);
        assert_eq!(
            dev.user_data,
            PathBuf::from("/home/u/.config/Muxus-development")
        );
        assert_eq!(
            dev.forced_history_root(),
            Some(PathBuf::from("/home/u/.config/Muxus-development/history"))
        );
        let mac = Environment::new(Platform::MacOs, "/Users/u");
        assert_eq!(
            AppPaths::installed(&mac).user_data,
            PathBuf::from("/Users/u/Library/Application Support/Muxus")
        );
    }

    #[test]
    fn server_defaults_follow_xdg_data_home() {
        let linux = Environment::new(Platform::Linux, "/home/u");
        assert_eq!(
            server_default_database_path(&linux),
            PathBuf::from("/home/u/.local/share/muxus/muxus.sqlite3")
        );
        let xdg = linux.with_var("XDG_DATA_HOME", "/data");
        assert_eq!(
            server_default_database_path(&xdg),
            PathBuf::from("/data/muxus/muxus.sqlite3")
        );
        assert_eq!(server_history_override(&xdg), None);
        assert_eq!(
            server_history_override(&xdg.clone().with_var("MUXUS_HISTORY_PATH", "/h")),
            Some("/h".into())
        );
    }

    #[test]
    fn history_root_prefers_forced_then_configured_then_default() {
        let db = Path::new("/d/muxus.sqlite3");
        assert_eq!(
            history_root(None, None, db),
            Some(PathBuf::from("/d/history"))
        );
        assert_eq!(
            history_root(None, Some("/h"), db),
            Some(PathBuf::from("/h"))
        );
        assert_eq!(
            history_root(Some(Path::new("/f")), Some("/h"), db),
            Some(PathBuf::from("/f"))
        );
        assert_eq!(default_history_root(Path::new(":memory:")), None);
    }
}
