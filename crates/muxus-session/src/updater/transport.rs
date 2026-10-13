//! What electron-updater did for the desktop updater: read the release feed, compare versions,
//! download the installer for this platform into a cache folder and verify it, and hand over
//! to it. [`UpdateTransport`] is the seam the [`DesktopUpdater`](super::DesktopUpdater) state
//! machine drives (and tests fake); [`FeedTransport`] is the real one.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use base64::Engine as _;
use muxus_core::platform::Platform;
use muxus_core::update_version::is_newer_version;
use sha2::Digest as _;

use super::feed::{
    UpdateFileInfo, UpdateInfo, channel_file_name, find_file, parse_update_info, resolve_file_url,
};
use super::http::{HttpError, MAX_DOCUMENT_BYTES, UpdateHttp};
use super::install::Installer;

/// Why an update step failed. The message goes to the log; the user sees a generic one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateError(pub String);

impl std::fmt::Display for UpdateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for UpdateError {}

impl From<HttpError> for UpdateError {
    fn from(error: HttpError) -> Self {
        UpdateError(error.to_string())
    }
}

impl From<String> for UpdateError {
    fn from(message: String) -> Self {
        UpdateError(message)
    }
}

/// The update service the desktop updater drives (electron-updater's `AppUpdater`).
#[async_trait]
pub trait UpdateTransport: Send + Sync {
    /// Ask the release feed: the newer version, or `None` when this build is current.
    async fn check(&self) -> Result<Option<String>, UpdateError>;

    /// Download and verify the update the last check found; `progress` gets percentages.
    /// Returns its version.
    async fn download(&self, progress: &(dyn Fn(f64) + Send + Sync))
    -> Result<String, UpdateError>;

    /// Install the downloaded update and start it (`quitAndInstall`). Runs once the backend
    /// has closed; the app exits right after.
    fn quit_and_install(&self) -> Result<(), UpdateError>;
}

/// The GitHub account and repository releases are published in. The desktop updater will move
/// to the Rust build's own releases once they exist.
pub const RELEASE_OWNER: &str = "FloSch62";
pub const RELEASE_REPO: &str = "muxus";

/// Where the release feeds live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReleaseFeed {
    /// The newest published GitHub release (no drafts or prereleases) of `owner/repo`, as
    /// electron-updater's GitHub provider finds it: `/releases/latest`, then the feed among
    /// that release's files.
    GitHub { owner: String, repo: String },
    /// A plain folder holding the feeds and installers (a mirror, or a loopback server).
    Generic { base_url: String },
}

impl ReleaseFeed {
    /// The Muxus releases ([`RELEASE_OWNER`]/[`RELEASE_REPO`]).
    pub fn muxus() -> Self {
        ReleaseFeed::GitHub {
            owner: RELEASE_OWNER.into(),
            repo: RELEASE_REPO.into(),
        }
    }
}

/// What a check found.
#[derive(Debug, Clone)]
struct Found {
    info: UpdateInfo,
    file: UpdateFileInfo,
    url: String,
}

/// Everything [`FeedTransport`] needs.
pub struct FeedTransportOptions {
    pub feed: ReleaseFeed,
    pub current_version: String,
    pub platform: Platform,
    /// Node's CPU name (`x64`, `arm64`): picks the feed and the installer.
    pub arch: String,
    /// Downloads go to `<cache_dir>/pending`.
    pub cache_dir: PathBuf,
    pub http: Arc<dyn UpdateHttp>,
    pub installer: Arc<dyn Installer>,
}

/// The release-feed updater.
pub struct FeedTransport {
    options: FeedTransportOptions,
    found: Mutex<Option<Found>>,
    downloaded: Mutex<Option<PathBuf>>,
}

const FEED_ACCEPT: &str = "application/x-yaml, application/yaml, text/yaml, application/json, */*";

impl FeedTransport {
    pub fn new(options: FeedTransportOptions) -> Self {
        Self {
            options,
            found: Mutex::new(None),
            downloaded: Mutex::new(None),
        }
    }

    /// The installer kind this platform updates from.
    fn installer_extension(&self) -> (&'static str, Option<&'static [&'static str]>) {
        match self.options.platform {
            Platform::Windows => ("exe", None),
            Platform::MacOs => ("zip", Some(&["pkg", "dmg"])),
            _ => ("AppImage", Some(&["rpm", "deb", "pacman"])),
        }
    }

    /// The URL of this platform's feed in the newest release.
    async fn feed_url(&self) -> Result<String, UpdateError> {
        let channel = channel_file_name(self.options.platform, &self.options.arch);
        match &self.options.feed {
            ReleaseFeed::Generic { base_url } => {
                Ok(format!("{}/{channel}", base_url.trim_end_matches('/')))
            }
            ReleaseFeed::GitHub { owner, repo } => {
                let latest = format!("https://github.com/{owner}/{repo}/releases/latest");
                let body = self
                    .options
                    .http
                    .get(&latest, "application/json", MAX_DOCUMENT_BYTES)
                    .await
                    .map_err(|error| match error {
                        HttpError::Status(404) => {
                            UpdateError("No published versions on GitHub".into())
                        }
                        other => other.into(),
                    })?;
                let release: serde_json::Value = serde_json::from_slice(&body)
                    .map_err(|_| UpdateError("Unable to find latest version on GitHub".into()))?;
                let tag = release
                    .get("tag_name")
                    .and_then(|tag| tag.as_str())
                    .filter(|tag| !tag.is_empty() && !tag.contains(['/', '?', '#']))
                    .ok_or_else(|| UpdateError("Unable to find latest version on GitHub".into()))?;
                Ok(format!(
                    "https://github.com/{owner}/{repo}/releases/download/{tag}/{channel}"
                ))
            }
        }
    }

    fn pending_dir(&self) -> PathBuf {
        self.options.cache_dir.join("pending")
    }
}

/// Base64 SHA-512 of a file.
pub fn file_sha512(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path)?;
    let mut hasher = sha2::Sha512::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(base64::engine::general_purpose::STANDARD.encode(hasher.finalize()))
}

/// The file name an installer URL ends in (percent-encoded spaces decoded).
fn file_name_of(url: &str) -> Option<String> {
    let path = url.split(['?', '#']).next()?;
    let name = path.rsplit('/').next()?.replace("%20", " ");
    (!name.is_empty() && name != "." && name != ".." && !name.contains(['\\', '\0']))
        .then_some(name)
}

#[async_trait]
impl UpdateTransport for FeedTransport {
    async fn check(&self) -> Result<Option<String>, UpdateError> {
        let url = self.feed_url().await?;
        let channel = channel_file_name(self.options.platform, &self.options.arch);
        let body = self
            .options
            .http
            .get(&url, FEED_ACCEPT, MAX_DOCUMENT_BYTES)
            .await
            .map_err(|error| match error {
                HttpError::Status(404) => UpdateError(format!(
                    "Cannot find {channel} in the latest release artifacts"
                )),
                other => other.into(),
            })?;
        let text = String::from_utf8_lossy(&body);
        let info = parse_update_info(&text)?;
        if !is_newer_version(&info.version, &self.options.current_version) {
            *self.found.lock().unwrap_or_else(|p| p.into_inner()) = None;
            return Ok(None);
        }
        let files = info.file_list()?;
        let (extension, not) = self.installer_extension();
        let file = find_file(&files, extension, not, &self.options.arch)
            .cloned()
            .ok_or_else(|| {
                UpdateError(format!("The release has no .{extension} for this platform"))
            })?;
        // Replacing an AppImage with a .deb (or any other kind) would break the installation.
        if !file
            .url
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .to_lowercase()
            .ends_with(&format!(".{}", extension.to_lowercase()))
        {
            return Err(UpdateError(format!(
                "The release has no .{extension} for this platform"
            )));
        }
        let file_url = resolve_file_url(&url, &file.url);
        let version = info.version.clone();
        *self.found.lock().unwrap_or_else(|p| p.into_inner()) = Some(Found {
            info,
            file,
            url: file_url,
        });
        Ok(Some(version))
    }

    async fn download(
        &self,
        progress: &(dyn Fn(f64) + Send + Sync),
    ) -> Result<String, UpdateError> {
        let found = self
            .found
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| UpdateError("Please check update first".into()))?;
        let name = file_name_of(&found.file.url)
            .ok_or_else(|| UpdateError(format!("Bad installer name: {}", found.file.url)))?;
        let pending = self.pending_dir();
        tokio::fs::create_dir_all(&pending).await.map_err(|error| {
            UpdateError(format!("Cannot create {}: {error}", pending.display()))
        })?;
        let target = pending.join(&name);
        let expected = found.file.sha512.clone();

        // A finished download from an earlier run is used again when it still verifies.
        let cached = target.clone();
        let reuse = tokio::task::spawn_blocking(move || file_sha512(&cached).ok())
            .await
            .ok()
            .flatten()
            .is_some_and(|actual| actual == expected);
        if !reuse {
            // Earlier, unfinished or outdated downloads.
            if let Ok(mut entries) = tokio::fs::read_dir(&pending).await {
                while let Ok(Some(entry)) = entries.next_entry().await {
                    let _ = tokio::fs::remove_file(entry.path()).await;
                }
            }
            let partial = pending.join(format!("{name}.part"));
            let size = found.file.size;
            let report = |received: u64, total: Option<u64>| {
                if let Some(total) = total.or(size).filter(|total| *total > 0) {
                    progress(received as f64 * 100.0 / total as f64);
                }
            };
            let result = self
                .options
                .http
                .download(&found.url, &partial, &report)
                .await;
            if let Err(error) = result {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(error.into());
            }
            let check = partial.clone();
            let (actual, length) = tokio::task::spawn_blocking(move || {
                (
                    file_sha512(&check).ok(),
                    std::fs::metadata(&check).map(|meta| meta.len()).ok(),
                )
            })
            .await
            .map_err(|error| UpdateError(error.to_string()))?;
            if let (Some(expected_size), Some(length)) = (size, length)
                && expected_size != length
            {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(UpdateError(format!(
                    "size mismatch, expected {expected_size}, got {length}"
                )));
            }
            if actual.as_deref() != Some(found.file.sha512.as_str()) {
                let _ = tokio::fs::remove_file(&partial).await;
                return Err(UpdateError(format!(
                    "sha512 checksum mismatch, expected {}, got {}",
                    found.file.sha512,
                    actual.unwrap_or_default()
                )));
            }
            tokio::fs::rename(&partial, &target)
                .await
                .map_err(|error| UpdateError(error.to_string()))?;
        }
        progress(100.0);
        *self.downloaded.lock().unwrap_or_else(|p| p.into_inner()) = Some(target);
        Ok(found.info.version)
    }

    fn quit_and_install(&self) -> Result<(), UpdateError> {
        let downloaded = self
            .downloaded
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .clone()
            .ok_or_else(|| UpdateError("No update has been downloaded".into()))?;
        self.options
            .installer
            .install(&downloaded)
            .map_err(UpdateError)
    }
}
