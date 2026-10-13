//! Handing over to a downloaded update (`quitAndInstall` in electron-updater): replace the
//! AppImage on Linux, run the installer on Windows, swap the app bundle on macOS, then start
//! the new version.
//!
//! The new process starts while this one is still shutting down. It gets
//! [`RELAUNCH_AFTER_PID_VAR`] with this process's id and waits for it to exit before it takes
//! the single-instance lock, so it never hands its launch to the dying instance.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Set on a relaunched update: the id of the process it replaces.
pub const RELAUNCH_AFTER_PID_VAR: &str = "MUXUS_RELAUNCH_AFTER_PID";

/// Installs a downloaded update and starts it.
pub trait Installer: Send + Sync {
    fn install(&self, downloaded: &Path) -> Result<(), String>;
}

/// How an installed update is started.
pub trait Launcher: Send + Sync {
    fn launch(
        &self,
        program: &Path,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<(), String>;
}

/// Starts the program detached from this process, with exactly `env`.
pub struct DetachedLauncher;

impl Launcher for DetachedLauncher {
    fn launch(
        &self,
        program: &Path,
        args: &[String],
        env: &HashMap<String, String>,
    ) -> Result<(), String> {
        let mut command = Command::new(program);
        command
            .args(args)
            .env_clear()
            .envs(env)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            // Its own session: the new version outlives this one and its terminal.
            command.process_group(0);
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const DETACHED_PROCESS: u32 = 0x0000_0008;
            const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
            command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
        }
        command
            .spawn()
            .map(|_| ())
            .map_err(|error| format!("Could not start {}: {error}", program.display()))
    }
}

/// The environment a relaunched version starts with: this process's, minus an AppImage's own
/// mount, plus the wait for this process.
pub fn relaunch_environment(base: &HashMap<String, String>) -> HashMap<String, String> {
    let mut env = crate::links::without_app_image_paths(base);
    env.insert(
        RELAUNCH_AFTER_PID_VAR.into(),
        std::process::id().to_string(),
    );
    env
}

fn has_version(name: &str) -> bool {
    static VERSION: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"\d+\.\d+\.\d+").expect("valid regex"));
    VERSION.is_match(name)
}

/// Where the new AppImage goes (`AppImageUpdater.doInstall`): over the running one when the
/// names match or the running one carries no version (the user renamed it and wants that
/// name kept), else next to it under the new file's versioned name.
pub fn app_image_destination(running: &Path, downloaded: &Path) -> PathBuf {
    let existing = running.file_name().unwrap_or_default();
    let incoming = downloaded.file_name().unwrap_or_default();
    if incoming == existing || !has_version(&existing.to_string_lossy()) {
        running.to_path_buf()
    } else {
        running
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(incoming)
    }
}

/// Put the downloaded AppImage in place of the running one; returns where it went.
///
/// The running AppImage stays mounted from its old inode, so replacing the file under it is
/// safe. Unlike electron-updater (which unlinks first), the new file is staged next to the
/// destination and renamed over it, so a failure never leaves the user without Muxus.
pub fn replace_app_image(running: &Path, downloaded: &Path) -> Result<PathBuf, String> {
    let destination = app_image_destination(running, downloaded);
    let folder = destination
        .parent()
        .ok_or_else(|| "The AppImage has no folder".to_string())?;
    let staged = folder.join(format!(
        ".{}.update-{}",
        destination
            .file_name()
            .unwrap_or_default()
            .to_string_lossy(),
        std::process::id()
    ));
    let _ = std::fs::remove_file(&staged);
    if std::fs::rename(downloaded, &staged).is_err() {
        // Another file system: copy, then drop the download.
        std::fs::copy(downloaded, &staged).map_err(|error| {
            let _ = std::fs::remove_file(&staged);
            format!("Could not copy the update to {}: {error}", folder.display())
        })?;
        let _ = std::fs::remove_file(downloaded);
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&staged, std::fs::Permissions::from_mode(0o755)).map_err(
            |error| {
                let _ = std::fs::remove_file(&staged);
                format!("Could not make the update executable: {error}")
            },
        )?;
    }
    std::fs::rename(&staged, &destination).map_err(|error| {
        let _ = std::fs::remove_file(&staged);
        format!("Could not replace {}: {error}", destination.display())
    })?;
    if destination != running {
        let _ = std::fs::remove_file(running);
    }
    Ok(destination)
}

/// Linux AppImage updates.
pub struct AppImageInstaller {
    /// The running AppImage (`$APPIMAGE`).
    pub app_image: PathBuf,
    /// This process's environment.
    pub env: HashMap<String, String>,
    pub launcher: Box<dyn Launcher>,
}

impl Installer for AppImageInstaller {
    fn install(&self, downloaded: &Path) -> Result<(), String> {
        let destination = replace_app_image(&self.app_image, downloaded)?;
        let mut env = relaunch_environment(&self.env);
        env.insert("APPIMAGE_SILENT_INSTALL".into(), "true".into());
        self.launcher.launch(&destination, &[], &env)
    }
}

/// Windows NSIS installers: they replace the installation and, with `--force-run`, start
/// Muxus again (electron-updater's `NsisUpdater` arguments).
pub struct NsisInstaller {
    pub env: HashMap<String, String>,
    pub launcher: Box<dyn Launcher>,
}

impl Installer for NsisInstaller {
    fn install(&self, downloaded: &Path) -> Result<(), String> {
        let env = relaunch_environment(&self.env);
        self.launcher.launch(
            downloaded,
            &["--updated".to_string(), "--force-run".to_string()],
            &env,
        )
    }
}

/// macOS zip updates: unpack the new bundle beside the running one, swap them, start it.
pub struct MacZipInstaller {
    /// The running `.app` bundle.
    pub bundle: PathBuf,
    pub env: HashMap<String, String>,
    pub launcher: Box<dyn Launcher>,
}

impl Installer for MacZipInstaller {
    fn install(&self, downloaded: &Path) -> Result<(), String> {
        let folder = self
            .bundle
            .parent()
            .ok_or_else(|| "The app bundle has no folder".to_string())?;
        let unpack = folder.join(format!(".muxus-update-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&unpack);
        let status = Command::new("/usr/bin/ditto")
            .args(["-x", "-k"])
            .arg(downloaded)
            .arg(&unpack)
            .status()
            .map_err(|error| format!("Could not unpack the update: {error}"))?;
        if !status.success() {
            let _ = std::fs::remove_dir_all(&unpack);
            return Err("Could not unpack the update".into());
        }
        let new_bundle = std::fs::read_dir(&unpack)
            .map_err(|error| error.to_string())?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
            .ok_or_else(|| "The update holds no app bundle".to_string())?;
        let backup = folder.join(format!(".muxus-previous-{}", std::process::id()));
        std::fs::rename(&self.bundle, &backup)
            .map_err(|error| format!("Could not move the running app aside: {error}"))?;
        if let Err(error) = std::fs::rename(&new_bundle, &self.bundle) {
            let _ = std::fs::rename(&backup, &self.bundle);
            let _ = std::fs::remove_dir_all(&unpack);
            return Err(format!("Could not install the update: {error}"));
        }
        let _ = std::fs::remove_dir_all(&backup);
        let _ = std::fs::remove_dir_all(&unpack);
        let env = relaunch_environment(&self.env);
        self.launcher.launch(
            Path::new("/usr/bin/open"),
            &["-n".to_string(), self.bundle.to_string_lossy().into_owned()],
            &env,
        )
    }
}

/// The `.app` bundle an executable runs from.
pub fn app_bundle_of(executable: &Path) -> Option<PathBuf> {
    executable
        .ancestors()
        .find(|path| path.extension().is_some_and(|ext| ext == "app"))
        .map(Path::to_path_buf)
}

/// Starts this program again (`app.relaunch()`), for an installer that failed after the
/// app began to quit.
pub fn relaunch_current(env: &HashMap<String, String>) -> Result<(), String> {
    let program = match env.get("APPIMAGE").filter(|path| !path.is_empty()) {
        Some(app_image) => PathBuf::from(app_image),
        None => std::env::current_exe().map_err(|error| error.to_string())?,
    };
    DetachedLauncher.launch(&program, &[], &relaunch_environment(env))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    type Launch = (PathBuf, Vec<String>, HashMap<String, String>);

    #[derive(Default)]
    struct Recorder(Mutex<Vec<Launch>>);

    impl Launcher for std::sync::Arc<Recorder> {
        fn launch(
            &self,
            program: &Path,
            args: &[String],
            env: &HashMap<String, String>,
        ) -> Result<(), String> {
            self.0
                .lock()
                .unwrap()
                .push((program.to_path_buf(), args.to_vec(), env.clone()));
            Ok(())
        }
    }

    #[test]
    fn keeps_a_renamed_appimage_name_and_follows_versioned_names() {
        let running = Path::new("/apps/Muxus.AppImage");
        assert_eq!(
            app_image_destination(
                running,
                Path::new("/cache/muxus-1.2.0-linux-x86_64.AppImage")
            ),
            running
        );
        let versioned = Path::new("/apps/muxus-1.1.0-linux-x86_64.AppImage");
        assert_eq!(
            app_image_destination(
                versioned,
                Path::new("/cache/muxus-1.2.0-linux-x86_64.AppImage")
            ),
            Path::new("/apps/muxus-1.2.0-linux-x86_64.AppImage")
        );
        let same = Path::new("/apps/update.AppImage");
        assert_eq!(
            app_image_destination(same, Path::new("/c/update.AppImage")),
            same
        );
    }

    #[test]
    fn replaces_the_appimage_and_starts_the_new_one_after_this_process() {
        let dir = tempfile::tempdir().unwrap();
        let running = dir.path().join("muxus-1.1.0-linux-x86_64.AppImage");
        std::fs::write(&running, b"old").unwrap();
        let cache = dir.path().join("cache");
        std::fs::create_dir(&cache).unwrap();
        let downloaded = cache.join("muxus-1.2.0-linux-x86_64.AppImage");
        std::fs::write(&downloaded, b"new").unwrap();
        let recorder = std::sync::Arc::new(Recorder::default());
        let mount = "/tmp/.mount_Muxus1";
        let env: HashMap<String, String> = [
            ("APPIMAGE", running.to_string_lossy().into_owned()),
            ("APPDIR", mount.to_string()),
            ("PATH", format!("{mount}/usr/bin:/usr/bin")),
            ("HOME", "/home/me".to_string()),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect();
        let installer = AppImageInstaller {
            app_image: running.clone(),
            env,
            launcher: Box::new(recorder.clone()),
        };
        installer.install(&downloaded).unwrap();
        let destination = dir.path().join("muxus-1.2.0-linux-x86_64.AppImage");
        assert_eq!(std::fs::read(&destination).unwrap(), b"new");
        assert!(!running.exists(), "the old versioned AppImage is removed");
        assert!(!downloaded.exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&destination)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
        }
        let launches = recorder.0.lock().unwrap();
        assert_eq!(launches.len(), 1);
        let (program, args, env) = &launches[0];
        assert_eq!(program, &destination);
        assert!(args.is_empty());
        assert_eq!(env["APPIMAGE_SILENT_INSTALL"], "true");
        assert_eq!(env[RELAUNCH_AFTER_PID_VAR], std::process::id().to_string());
        assert_eq!(
            env["PATH"], "/usr/bin",
            "the old mount stays out of the new process"
        );
        // Only staged files were in the folder; nothing is left behind.
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().starts_with('.'))
            .collect();
        assert!(leftovers.is_empty());
    }

    #[test]
    fn a_failed_replacement_keeps_the_running_appimage() {
        let dir = tempfile::tempdir().unwrap();
        let running = dir.path().join("Muxus.AppImage");
        std::fs::write(&running, b"old").unwrap();
        let missing = dir.path().join("gone.AppImage");
        assert!(replace_app_image(&running, &missing).is_err());
        assert_eq!(std::fs::read(&running).unwrap(), b"old");
    }

    #[test]
    fn runs_nsis_installers_with_the_updater_arguments() {
        let recorder = std::sync::Arc::new(Recorder::default());
        let installer = NsisInstaller {
            env: HashMap::new(),
            launcher: Box::new(recorder.clone()),
        };
        installer
            .install(Path::new("C:/cache/muxus-1.2.0-win-x64.exe"))
            .unwrap();
        let launches = recorder.0.lock().unwrap();
        assert_eq!(launches[0].1, ["--updated", "--force-run"]);
    }

    #[test]
    fn finds_the_app_bundle() {
        assert_eq!(
            app_bundle_of(Path::new("/Applications/Muxus.app/Contents/MacOS/muxus")).unwrap(),
            Path::new("/Applications/Muxus.app")
        );
        assert_eq!(app_bundle_of(Path::new("/usr/bin/muxus")), None);
    }
}
