//! App information, X11 settings and the update check (`server/src/routes/app.ts`), the log
//! viewer (`routes/logs.ts`), local shell discovery (`routes/local-shells.ts`) and serial ports
//! (`routes/serial.ts`).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Empty, Limited};
use hyper::body::Bytes;
use muxus_core::api::{
    AppInfo, AppLogsResponse, SerialPortsResponse, UpdateCheckResult, WslDistributionsResponse,
    X11SettingsUpdate, X11Status,
};
use muxus_core::update_version::is_newer_version;
use serde::{Deserialize, Serialize};

use crate::backend::Backend;
use crate::error::{ApiError, ApiResult};
use crate::logging::app_log::{APP_LOG_CAPACITY, app_log_entries, clear_app_log};
use crate::logging::level::{debug_enabled, set_debug_enabled};

const UPDATE_CHECK_TIMEOUT: Duration = Duration::from_secs(10);
const MAX_MANIFEST_BYTES: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;

/// Answer of `PUT /api/logs/settings`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LogSettingsResponse {
    pub debug_enabled: bool,
}

impl Backend {
    /// `GET /api/app/info`.
    pub async fn app_info(&self) -> ApiResult<AppInfo> {
        let shared = self.shared.clone();
        self.blocking(move |_| {
            let environment = shared.local_environment();
            let ssh_algorithms: BTreeMap<String, Vec<String>> =
                muxus_ssh::algorithms::supported_algorithms()
                    .into_iter()
                    .map(|(keyword, names)| {
                        (
                            keyword.to_string(),
                            names.into_iter().map(str::to_string).collect(),
                        )
                    })
                    .collect();
            Ok(AppInfo {
                name: "Muxus".into(),
                version: shared.config.app_version.clone(),
                platform: environment.platform.node_name().into(),
                home_dir: shared.config.home.to_string_lossy().into_owned(),
                default_shell: muxus_local::pty::default_shell(
                    &environment.vars,
                    environment.platform,
                ),
                ssh_algorithms,
            })
        })
        .await
    }

    /// `GET /api/x11`: live, so installing an X server or changing DISPLAY shows without a
    /// restart.
    pub async fn x11_status(&self) -> ApiResult<X11Status> {
        self.run(|shared| async move { Ok(shared.x11.status().await) })
            .await
    }

    /// `PUT /api/x11/settings`: the settings live in the UI's preferences and are sent on boot
    /// and on every change.
    pub async fn put_x11_settings(&self, settings: X11SettingsUpdate) -> ApiResult<X11Status> {
        self.run(move |shared| async move {
            shared.x11.apply_settings(settings);
            Ok(shared.x11.status().await)
        })
        .await
    }

    /// `GET /api/app/update-check`: one check per run, shared by every window; `force` starts
    /// a fresh one that bypasses caches.
    pub async fn update_check(&self, force: bool) -> ApiResult<UpdateCheckResult> {
        self.run(move |shared| async move {
            let cell = {
                let mut slot = shared
                    .update_check
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if force {
                    *slot = Arc::new(tokio::sync::OnceCell::new());
                }
                slot.clone()
            };
            let url = shared.config.update_manifest_url.clone();
            let version = shared.config.app_version.clone();
            Ok(cell
                .get_or_init(|| check_for_update(url, version, force))
                .await
                .clone())
        })
        .await
    }

    /// `GET /api/logs`.
    pub async fn app_logs(&self) -> ApiResult<AppLogsResponse> {
        Ok(AppLogsResponse {
            entries: app_log_entries(),
            debug_enabled: debug_enabled(),
            capacity: APP_LOG_CAPACITY as u32,
        })
    }

    /// `PUT /api/logs/settings`.
    pub async fn put_log_settings(&self, debug_logging: bool) -> ApiResult<LogSettingsResponse> {
        if set_debug_enabled(debug_logging) {
            let message = if debug_logging {
                "debug logging enabled"
            } else {
                "debug logging disabled"
            };
            tracing::info!("{message}");
        }
        Ok(LogSettingsResponse {
            debug_enabled: debug_enabled(),
        })
    }

    /// `DELETE /api/logs`.
    pub async fn clear_app_logs(&self) -> ApiResult<bool> {
        clear_app_log();
        Ok(true)
    }

    /// `GET /api/local-shells/wsl`: read on every request, so a distribution installed or
    /// removed while Muxus runs shows up the next time the list is asked for.
    pub async fn wsl_distributions(&self) -> ApiResult<WslDistributionsResponse> {
        self.blocking(|_| {
            Ok(WslDistributionsResponse {
                distributions: muxus_local::wsl::list_wsl_distributions(),
            })
        })
        .await
    }

    /// `GET /api/serial/ports`.
    pub async fn serial_ports(&self) -> ApiResult<SerialPortsResponse> {
        self.spawn(async {
            muxus_serial::list_ports()
                .await
                .map_err(|error| ApiError::internal(error.to_string()))
        })
        .await
    }
}

fn normalize_version(version: &str) -> String {
    let trimmed = version.trim();
    match trimmed.get(..1) {
        Some("v" | "V") => trimmed[1..].to_string(),
        _ => trimmed.to_string(),
    }
}

/// The release page, when it is a GitHub release of Muxus over https.
fn release_url(value: Option<&serde_json::Value>) -> Option<String> {
    let text = value?.as_str()?;
    let uri: hyper::Uri = text.parse().ok()?;
    if uri.scheme_str() != Some("https") || uri.host() != Some("github.com") {
        return None;
    }
    if uri.port().is_some_and(|port| port.as_u16() != 443) {
        return None;
    }
    if !uri.path().starts_with("/FloSch62/muxus/releases/") {
        return None;
    }
    Some(text.to_string())
}

/// Why fetching the manifest failed.
enum FetchError {
    Timeout,
    Network,
}

/// The update check's reason when no release manifest is configured.
pub const UPDATES_DISABLED_REASON: &str = "disabled";

async fn check_for_update(url: String, current_version: String, force: bool) -> UpdateCheckResult {
    let not_available = |latest: Option<String>, reason: &str| UpdateCheckResult::NotAvailable {
        current_version: current_version.clone(),
        latest_version: latest,
        reason: Some(reason.to_string()),
    };
    // No release feed (a build whose releases are not published yet): a neutral answer
    // instead of offering another build's releases.
    if url.trim().is_empty() {
        return not_available(None, UPDATES_DISABLED_REASON);
    }
    let url = if force {
        let separator = if url.contains('?') { '&' } else { '?' };
        format!(
            "{url}{separator}t={}",
            chrono::Utc::now().timestamp_millis()
        )
    } else {
        url
    };
    let fetched = tokio::time::timeout(
        UPDATE_CHECK_TIMEOUT,
        fetch(&url, &format!("Muxus/{current_version}")),
    )
    .await
    .unwrap_or(Err(FetchError::Timeout));
    let (status, body) = match fetched {
        Ok(response) => response,
        Err(FetchError::Timeout) => return not_available(None, "timeout"),
        Err(FetchError::Network) => return not_available(None, "network"),
    };
    if status == 404 {
        return not_available(None, "no-release");
    }
    if !(200..300).contains(&status) {
        return not_available(None, &format!("manifest-{status}"));
    }
    let Ok(manifest) = serde_json::from_slice::<serde_json::Value>(&body) else {
        return not_available(None, "network");
    };
    let Some(version) = manifest.get("version").and_then(|value| value.as_str()) else {
        return not_available(None, "missing-version");
    };
    let latest_version = normalize_version(version);
    if !is_newer_version(&latest_version, &current_version) {
        return UpdateCheckResult::NotAvailable {
            current_version,
            latest_version: Some(latest_version),
            reason: None,
        };
    }
    let Some(release_url) = release_url(manifest.get("releaseUrl")) else {
        return not_available(Some(latest_version), "missing-release-url");
    };
    UpdateCheckResult::Available {
        current_version,
        latest_version,
        release_name: manifest
            .get("releaseName")
            .and_then(|value| value.as_str())
            .filter(|name| !name.is_empty())
            .map(str::to_string),
        release_url,
        published_at: manifest
            .get("publishedAt")
            .and_then(|value| value.as_str())
            .map(str::to_string),
    }
}

/// GET `url` (http or https, following redirects): status and body.
async fn fetch(url: &str, user_agent: &str) -> Result<(u16, Vec<u8>), FetchError> {
    let mut uri: hyper::Uri = url.parse().map_err(|_| FetchError::Network)?;
    for _ in 0..=MAX_REDIRECTS {
        let (status, location, body) = fetch_once(&uri, user_agent).await?;
        match location {
            Some(location) if (300..400).contains(&status) => {
                uri = resolve_location(&uri, &location).ok_or(FetchError::Network)?;
            }
            _ => return Ok((status, body)),
        }
    }
    Err(FetchError::Network)
}

fn resolve_location(base: &hyper::Uri, location: &str) -> Option<hyper::Uri> {
    if location.starts_with("http://") || location.starts_with("https://") {
        return location.parse().ok();
    }
    let authority = base.authority()?.as_str();
    let scheme = base.scheme_str()?;
    format!("{scheme}://{authority}{location}").parse().ok()
}

async fn fetch_once(
    uri: &hyper::Uri,
    user_agent: &str,
) -> Result<(u16, Option<String>, Vec<u8>), FetchError> {
    let https = match uri.scheme_str() {
        Some("https") => true,
        Some("http") => false,
        _ => return Err(FetchError::Network),
    };
    let host = uri.host().ok_or(FetchError::Network)?.to_string();
    let port = uri.port_u16().unwrap_or(if https { 443 } else { 80 });
    let tcp = tokio::net::TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|_| FetchError::Network)?;
    let _ = tcp.set_nodelay(true);
    let path = uri
        .path_and_query()
        .map(|path| path.as_str().to_string())
        .unwrap_or_else(|| "/".into());
    let request = hyper::Request::get(path)
        .header(
            hyper::header::HOST,
            uri.authority().map(|a| a.as_str()).unwrap_or(&host),
        )
        .header(hyper::header::ACCEPT, "application/json")
        .header(hyper::header::USER_AGENT, user_agent)
        .body(Empty::<Bytes>::new())
        .map_err(|_| FetchError::Network)?;
    if https {
        let connector = tokio_rustls::TlsConnector::from(Arc::new(tls_config()));
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|_| FetchError::Network)?;
        let tls = connector
            .connect(name, tcp)
            .await
            .map_err(|_| FetchError::Network)?;
        send(hyper_util::rt::TokioIo::new(tls), request).await
    } else {
        send(hyper_util::rt::TokioIo::new(tcp), request).await
    }
}

async fn send<T>(
    io: T,
    request: hyper::Request<Empty<Bytes>>,
) -> Result<(u16, Option<String>, Vec<u8>), FetchError>
where
    T: hyper::rt::Read + hyper::rt::Write + Unpin + Send + 'static,
{
    let (mut sender, connection) = hyper::client::conn::http1::handshake(io)
        .await
        .map_err(|_| FetchError::Network)?;
    tokio::spawn(async move {
        let _ = connection.await;
    });
    let response = sender
        .send_request(request)
        .await
        .map_err(|_| FetchError::Network)?;
    let status = response.status().as_u16();
    let location = response
        .headers()
        .get(hyper::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let body = Limited::new(response.into_body(), MAX_MANIFEST_BYTES)
        .collect()
        .await
        .map_err(|_| FetchError::Network)?
        .to_bytes()
        .to_vec();
    Ok((status, location, body))
}

fn tls_config() -> rustls::ClientConfig {
    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .expect("the default protocol versions")
        .with_root_certificates(roots)
        .with_no_client_auth()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_urls_must_be_muxus_releases_on_github() {
        let ok = serde_json::json!("https://github.com/FloSch62/muxus/releases/tag/v0.12.0");
        assert_eq!(
            release_url(Some(&ok)).as_deref(),
            Some("https://github.com/FloSch62/muxus/releases/tag/v0.12.0")
        );
        for bad in [
            "https://attacker.example/FloSch62/muxus/releases/tag/v0.12.0",
            "http://github.com/FloSch62/muxus/releases/tag/v0.12.0",
            "https://github.com/other/muxus/releases/tag/v1",
        ] {
            assert_eq!(release_url(Some(&serde_json::json!(bad))), None, "{bad}");
        }
        assert_eq!(normalize_version(" v0.12.0 "), "0.12.0");
    }
}
