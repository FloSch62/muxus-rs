//! Session history, logging policies and log file settings
//! (`server/src/routes/session-history.ts`).

use std::path::Path;

use base64::Engine;
use muxus_core::api::{
    SessionHistoryResponse, SessionHistorySettings, SessionHistoryStorageStatus, SessionLogDetail,
    SessionLogFileSettings, SessionLogFileStatus, SessionLoggingPolicy, SessionLoggingPolicyInput,
    session_log_file_pattern_error, session_log_name,
};
use muxus_core::js::{js_trim, parse_timestamp, utf16_len};
use muxus_core::protocol::TerminalProfile;
use muxus_core::session_transcript::session_transcript;
use muxus_core::validate::Validate;
use muxus_store::history::SessionHistoryQuery;
use muxus_store::paths::default_history_root;
use serde::Serialize;

use crate::backend::{Backend, Shared};
use crate::error::{ApiError, ApiResult};
use crate::logging::{
    session_log_directory, session_log_file_path, session_profile_identity,
    unused_session_log_file_path,
};

/// Events a session preview shows (`TRANSCRIPT_PREVIEW_EVENTS`).
const TRANSCRIPT_PREVIEW_EVENTS: usize = 5_000;

/// A file export: what the route answered with an attachment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionLogExport {
    /// The attachment's file name.
    pub file_name: String,
    pub content_type: &'static str,
    pub body: Vec<u8>,
}

fn not_found() -> ApiError {
    ApiError::not_found("session log not found")
}

fn too_big(limit: usize) -> ApiError {
    ApiError::bad_request(format!(
        "Too big: expected string to have <={limit} characters"
    ))
}

fn check_trimmed(value: &mut Option<String>, max: usize, min: usize) -> ApiResult<()> {
    if let Some(text) = value {
        let trimmed = js_trim(text).to_string();
        let length = utf16_len(&trimmed);
        if length > max {
            return Err(too_big(max));
        }
        if length < min {
            return Err(ApiError::bad_request(format!(
                "Too small: expected string to have >={min} characters"
            )));
        }
        *text = trimmed;
    }
    Ok(())
}

/// `z.iso.datetime()`: an ISO 8601 UTC timestamp.
fn check_datetime(value: &Option<String>) -> ApiResult<()> {
    match value {
        Some(text)
            if !(text.ends_with('Z') && text.contains('T') && parse_timestamp(text).is_some()) =>
        {
            Err(ApiError::bad_request("Invalid ISO datetime"))
        }
        _ => Ok(()),
    }
}

fn is_absolute(path: &str) -> bool {
    Path::new(path).is_absolute()
}

/// Where the history is configured to live (`configuredStorageLocation`).
fn configured_storage_location(
    shared: &Shared,
    settings: &SessionHistorySettings,
) -> Option<String> {
    shared
        .config
        .history_path
        .as_ref()
        .map(|path| path.to_string_lossy().into_owned())
        .or_else(|| settings.storage_location.clone())
        .or_else(|| {
            default_history_root(&shared.config.database_path)
                .map(|path| path.to_string_lossy().into_owned())
        })
}

fn log_file_status(shared: &Shared, settings: SessionLogFileSettings) -> SessionLogFileStatus {
    SessionLogFileStatus {
        active_directory: session_log_directory(&settings, &shared.config.home)
            .to_string_lossy()
            .into_owned(),
        settings,
    }
}

fn check_profile_key(profile_key: &str) -> ApiResult<String> {
    let mut key = Some(profile_key.to_string());
    check_trimmed(&mut key, 500, 1)?;
    Ok(key.unwrap_or_default())
}

/// A file-name-safe slug of a session's name (`exportSlug`).
pub fn export_slug(title: &str) -> String {
    let lower = title.to_lowercase();
    let mut slug = String::new();
    let mut dash = false;
    for c in lower.chars() {
        if c.is_ascii_lowercase() || c.is_ascii_digit() {
            // Runs of other characters become one dash; leading and trailing ones vanish.
            if dash && !slug.is_empty() {
                slug.push('-');
            }
            dash = false;
            slug.push(c);
        } else {
            dash = true;
        }
    }
    let slug: String = slug.chars().take(48).collect();
    if slug.is_empty() {
        "session".into()
    } else {
        slug
    }
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Serialize)]
struct RawEvent<'a> {
    sequence: u64,
    #[serde(rename = "recordedAt")]
    recorded_at: &'a str,
    #[serde(rename = "elapsedMs")]
    elapsed_ms: u64,
    direction: muxus_core::api::SessionLogDirection,
    encoding: &'static str,
    data: String,
}

#[derive(Serialize)]
struct ReplayEvent<'a> {
    t: u64,
    d: muxus_core::api::SessionLogDirection,
    x: &'a str,
}

/// The standalone replay page (`sendReplay`).
pub fn replay_html(session: &SessionLogDetail) -> String {
    let events: Vec<ReplayEvent<'_>> = session
        .events
        .iter()
        .map(|event| ReplayEvent {
            t: event.elapsed_ms,
            d: event.direction,
            x: &event.text,
        })
        .collect();
    let events_json = serde_json::to_string(&events)
        .unwrap_or_else(|_| "[]".into())
        .replace('<', "\\u003c")
        .replace('\u{2028}', "\\u2028")
        .replace('\u{2029}', "\\u2029");
    let summary = &session.summary;
    let title = escape_html(session_log_name(&summary.title, summary.label.as_deref()));
    let host = escape_html(&summary.host);
    let started = escape_html(&summary.started_at);
    let event_count = summary.event_count;
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>{title} — Muxus replay</title>
<style>
:root{{color-scheme:dark;background:#0c1117;color:#d7dee7;font:14px Inter,system-ui,sans-serif}}
*{{box-sizing:border-box}}body{{margin:0;height:100vh;display:flex;flex-direction:column}}
header{{padding:14px 18px;background:#121a23;border-bottom:1px solid #293442}}
h1{{font-size:16px;margin:0 0 4px}}.meta{{color:#8fa0b3;font-size:12px}}
.controls{{display:flex;align-items:center;gap:10px;padding:10px 18px;background:#0f171f;border-bottom:1px solid #293442}}
button,select{{color:inherit;background:#1b2733;border:1px solid #3a4959;border-radius:5px;padding:6px 10px}}
input[type=range]{{flex:1}}.time{{font-variant-numeric:tabular-nums;min-width:110px;text-align:right;color:#aab8c6}}
pre{{flex:1;margin:0;padding:18px;overflow:auto;white-space:pre-wrap;word-break:break-word;font:13px/1.5 ui-monospace,SFMono-Regular,Consolas,monospace}}
.input{{color:#8ec7ff}}.system{{color:#8391a2;font-style:italic}}
</style>
</head>
<body>
<header><h1>{title}</h1><div class="meta">{host} · started {started} · {event_count} retained events</div></header>
<div class="controls">
  <button id="play" type="button">Play</button>
  <button id="restart" type="button">Restart</button>
  <label>Speed <select id="speed"><option value="0.5">0.5×</option><option value="1" selected>1×</option><option value="2">2×</option><option value="5">5×</option><option value="20">20×</option></select></label>
  <input id="seek" type="range" min="0" max="0" value="0" aria-label="Replay position">
  <span class="time" id="time">00:00 / 00:00</span>
</div>
<pre id="screen" aria-live="off"></pre>
<script>
const events={events_json};
const duration=events.length?events[events.length-1].t:0;
const screen=document.querySelector('#screen'),play=document.querySelector('#play'),seek=document.querySelector('#seek'),time=document.querySelector('#time'),speed=document.querySelector('#speed');
seek.max=String(duration);let position=0,index=0,running=false,last=0;
const fmt=ms=>{{const s=Math.floor(ms/1000),m=Math.floor(s/60);return String(m).padStart(2,'0')+':'+String(s%60).padStart(2,'0')}};
function append(e){{const span=document.createElement('span');span.className=e.d;span.textContent=e.x;screen.append(span)}}
function render(to){{screen.textContent='';index=0;while(index<events.length&&events[index].t<=to)append(events[index++]);screen.scrollTop=screen.scrollHeight;position=to;seek.value=String(to);time.textContent=fmt(to)+' / '+fmt(duration)}}
function frame(now){{if(!running)return;const delta=(now-last)*Number(speed.value);last=now;position=Math.min(duration,position+delta);while(index<events.length&&events[index].t<=position)append(events[index++]);screen.scrollTop=screen.scrollHeight;seek.value=String(position);time.textContent=fmt(position)+' / '+fmt(duration);if(position>=duration){{running=false;play.textContent='Play';return}}requestAnimationFrame(frame)}}
play.onclick=()=>{{running=!running;play.textContent=running?'Pause':'Play';if(running){{last=performance.now();requestAnimationFrame(frame)}}}};
document.querySelector('#restart').onclick=()=>{{running=false;play.textContent='Play';render(0)}};
seek.oninput=()=>render(Number(seek.value));render(0);
</script>
</body>
</html>"#
    )
}

impl Backend {
    /// `GET /api/session-history`: one page of sessions, newest first.
    pub async fn session_history(
        &self,
        query: SessionHistoryQuery,
    ) -> ApiResult<SessionHistoryResponse> {
        let mut query = query;
        check_trimmed(&mut query.query, 500, 0)?;
        check_trimmed(&mut query.profile_key, 500, 1)?;
        check_trimmed(&mut query.host, 500, 0)?;
        if query
            .kind
            .as_deref()
            .is_some_and(|kind| !matches!(kind, "ssh" | "local" | "serial" | "telnet"))
        {
            return Err(ApiError::bad_request(
                "Invalid option: expected one of \"ssh\"|\"local\"|\"serial\"|\"telnet\"",
            ));
        }
        check_datetime(&query.started_after)?;
        check_datetime(&query.started_before)?;
        if query.limit > 100 {
            return Err(ApiError::bad_request(
                "Too big: expected number to be <=100",
            ));
        }
        if query.limit == 0 {
            query.limit = 50;
        }
        if query
            .cursor
            .as_deref()
            .is_some_and(|cursor| utf16_len(cursor) > 1000)
        {
            return Err(too_big(1000));
        }
        self.run(move |shared| async move { Ok(shared.history.session_history(query).await?) })
            .await
    }

    /// `GET /api/session-history/storage`.
    pub async fn session_history_storage(&self) -> ApiResult<SessionHistoryStorageStatus> {
        self.run(|shared| async move {
            let database = shared.database.clone();
            let settings = tokio::task::spawn_blocking(move || database.session_history_settings())
                .await
                .map_err(|error| ApiError::internal(error.to_string()))??;
            let location = configured_storage_location(&shared, &settings);
            Ok(shared.history.storage_status(location.as_deref()).await?)
        })
        .await
    }

    /// `PUT /api/session-history/storage`.
    pub async fn put_session_history_storage(
        &self,
        settings: SessionHistorySettings,
    ) -> ApiResult<SessionHistoryStorageStatus> {
        let mut settings = settings;
        check_trimmed(&mut settings.storage_location, 4096, 0)?;
        if settings.storage_location.as_deref() == Some("") {
            settings.storage_location = None;
        }
        if settings.max_total_bytes < 64 * 1024 * 1024 {
            return Err(ApiError::bad_request(format!(
                "Too small: expected number to be >={}",
                64 * 1024 * 1024
            )));
        }
        if !(0.0..=100.0).contains(&settings.min_free_percent) {
            return Err(ApiError::bad_request(if settings.min_free_percent < 0.0 {
                "Too small: expected number to be >=0"
            } else {
                "Too big: expected number to be <=100"
            }));
        }
        if settings.max_age_days == Some(0) {
            return Err(ApiError::bad_request(
                "Too small: expected number to be >=1",
            ));
        }
        if let Some(location) = &settings.storage_location {
            if !is_absolute(location) {
                return Err(ApiError::bad_request(
                    "storageLocation must be an absolute path",
                ));
            }
            let resolved = std::path::absolute(location).unwrap_or_else(|_| location.into());
            if resolved.parent().is_none() {
                return Err(ApiError::bad_request(
                    "storageLocation cannot be the filesystem root",
                ));
            }
        }
        self.run(move |shared| async move {
            let database = shared.database.clone();
            let saved = tokio::task::spawn_blocking(move || {
                database.save_session_history_settings(&settings)
            })
            .await
            .map_err(|error| ApiError::internal(error.to_string()))??;
            shared.history.update_settings(saved.clone()).await?;
            let location = configured_storage_location(&shared, &saved);
            Ok(shared.history.storage_status(location.as_deref()).await?)
        })
        .await
    }

    /// `GET /api/session-history/log-files`.
    pub async fn session_log_files(&self) -> ApiResult<SessionLogFileStatus> {
        self.blocking(|shared| {
            Ok(log_file_status(
                shared,
                shared.database.session_log_file_settings()?,
            ))
        })
        .await
    }

    /// `PUT /api/session-history/log-files`.
    pub async fn put_session_log_files(
        &self,
        settings: SessionLogFileSettings,
    ) -> ApiResult<SessionLogFileStatus> {
        let mut settings = settings;
        check_trimmed(&mut settings.directory, 4096, 0)?;
        let mut pattern = Some(settings.filename_pattern.clone());
        check_trimmed(&mut pattern, 200, 0)?;
        settings.filename_pattern = pattern.unwrap_or_default();
        if let Some(directory) = settings.directory.as_deref()
            && !directory.is_empty()
            && !is_absolute(directory)
        {
            return Err(ApiError::bad_request(
                "The log folder must be an absolute path.",
            ));
        }
        if let Some(error) = session_log_file_pattern_error(&settings.filename_pattern) {
            return Err(ApiError::bad_request(error));
        }
        if settings.directory.as_deref() == Some("") {
            settings.directory = None;
        }
        self.blocking(move |shared| {
            Ok(log_file_status(
                shared,
                shared.database.save_session_log_file_settings(&settings)?,
            ))
        })
        .await
    }

    /// `POST /api/session-history/log-files/suggest`: a free file name for a log the user is
    /// about to start, to offer for editing.
    pub async fn suggest_session_log_file(
        &self,
        profile: TerminalProfile,
        title: Option<&str>,
    ) -> ApiResult<String> {
        let mut profile = profile;
        profile.validate().map_err(ApiError::invalid)?;
        let mut title = title.map(str::to_string);
        check_trimmed(&mut title, 500, 0)?;
        self.blocking(move |shared| {
            let identity = session_profile_identity(&profile);
            let settings = shared.database.session_log_file_settings()?;
            let title = title
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| identity.host.clone());
            let file = session_log_file_path(
                &settings,
                &shared.config.home,
                &identity.host,
                &title,
                identity.kind.as_str(),
                chrono::Local::now().naive_local(),
            );
            Ok(unused_session_log_file_path(&file)
                .to_string_lossy()
                .into_owned())
        })
        .await
    }

    /// `GET /api/session-history/policy`.
    pub async fn session_logging_policy(
        &self,
        profile_key: &str,
    ) -> ApiResult<SessionLoggingPolicy> {
        let key = check_profile_key(profile_key)?;
        self.blocking(move |shared| Ok(shared.database.session_logging_policy(&key)?))
            .await
    }

    /// `PUT /api/session-history/policy`.
    pub async fn put_session_logging_policy(
        &self,
        profile_key: &str,
        input: SessionLoggingPolicyInput,
    ) -> ApiResult<SessionLoggingPolicy> {
        let key = check_profile_key(profile_key)?;
        if input.max_part_bytes < 64 * 1024 {
            return Err(ApiError::bad_request(format!(
                "Too small: expected number to be >={}",
                64 * 1024
            )));
        }
        if input.max_part_bytes > 1024 * 1024 * 1024 {
            return Err(ApiError::bad_request(format!(
                "Too big: expected number to be <={}",
                1024 * 1024 * 1024
            )));
        }
        if input.max_parts < 1 {
            return Err(ApiError::bad_request(
                "Too small: expected number to be >=1",
            ));
        }
        if input.max_parts > 1000 {
            return Err(ApiError::bad_request(
                "Too big: expected number to be <=1000",
            ));
        }
        let input = SessionLoggingPolicyInput {
            log_to_file: Some(input.log_to_file.unwrap_or(false)),
            ..input
        };
        self.blocking(move |shared| {
            Ok(shared.database.save_session_logging_policy(&key, &input)?)
        })
        .await
    }

    /// `DELETE /api/session-history/policy`.
    pub async fn delete_session_logging_policy(&self, profile_key: &str) -> ApiResult<bool> {
        let key = check_profile_key(profile_key)?;
        self.blocking(move |shared| Ok(shared.database.delete_session_logging_policy(&key)?))
            .await
    }

    /// `GET /api/session-history/:id`: a session with its newest transcript events, or the
    /// window around the first match of `query`.
    pub async fn session_log(&self, id: &str, query: Option<&str>) -> ApiResult<SessionLogDetail> {
        let mut query = query.map(str::to_string);
        check_trimmed(&mut query, 500, 0)?;
        let id = id.to_string();
        self.run(move |shared| async move {
            let query = query.filter(|query| !query.is_empty());
            shared
                .history
                .session_log(&id, Some(TRANSCRIPT_PREVIEW_EVENTS), query.as_deref())
                .await?
                .ok_or_else(not_found)
        })
        .await
    }

    /// `GET /api/session-history/:id/raw`: every retained raw event as NDJSON.
    pub async fn export_session_log_raw(&self, id: &str) -> ApiResult<SessionLogExport> {
        let id = id.to_string();
        self.run(move |shared| async move {
            let (session, events) = tokio::join!(
                shared.history.session_log(&id, Some(0), None),
                shared.history.raw_session_log_events(&id)
            );
            let (Some(session), Some(events)) = (session?, events?) else {
                return Err(not_found());
            };
            let mut body = String::new();
            for event in &events {
                let line = serde_json::to_string(&RawEvent {
                    sequence: event.sequence,
                    recorded_at: &event.recorded_at,
                    elapsed_ms: event.elapsed_ms,
                    direction: event.direction,
                    encoding: "base64",
                    data: base64::engine::general_purpose::STANDARD.encode(&event.raw),
                })
                .map_err(|error| ApiError::internal(error.to_string()))?;
                body.push_str(&line);
                body.push('\n');
            }
            let summary = &session.summary;
            Ok(SessionLogExport {
                file_name: format!(
                    "{}.muxlog",
                    export_slug(session_log_name(&summary.title, summary.label.as_deref()))
                ),
                content_type: "application/x-ndjson",
                body: body.into_bytes(),
            })
        })
        .await
    }

    /// `GET /api/session-history/:id/clean`: the plain-text transcript.
    pub async fn export_session_log_clean(
        &self,
        id: &str,
        timestamps: bool,
    ) -> ApiResult<SessionLogExport> {
        let id = id.to_string();
        self.run(move |shared| async move {
            let session = shared
                .history
                .session_log(&id, None, None)
                .await?
                .ok_or_else(not_found)?;
            let transcript = session_transcript(&session.events, timestamps, false).text;
            let summary = &session.summary;
            Ok(SessionLogExport {
                file_name: format!(
                    "{}-clean.txt",
                    export_slug(session_log_name(&summary.title, summary.label.as_deref()))
                ),
                content_type: "text/plain; charset=utf-8",
                body: transcript.into_bytes(),
            })
        })
        .await
    }

    /// `GET /api/session-history/:id/replay.html`: a standalone replay page.
    pub async fn export_session_log_replay(&self, id: &str) -> ApiResult<SessionLogExport> {
        let id = id.to_string();
        self.run(move |shared| async move {
            let session = shared
                .history
                .session_log(&id, None, None)
                .await?
                .ok_or_else(not_found)?;
            let summary = &session.summary;
            Ok(SessionLogExport {
                file_name: format!(
                    "{}-replay.html",
                    export_slug(session_log_name(&summary.title, summary.label.as_deref()))
                ),
                content_type: "text/html",
                body: replay_html(&session).into_bytes(),
            })
        })
        .await
    }

    /// `PUT /api/session-history/:id/pin`.
    pub async fn set_session_log_pinned(&self, id: &str, pinned: bool) -> ApiResult<bool> {
        let id = id.to_string();
        self.run(move |shared| async move { Ok(shared.history.set_pinned(&id, pinned).await?) })
            .await
    }

    /// `PUT /api/session-history/:id/label`; an empty label shows the recorded title again.
    pub async fn set_session_log_label(&self, id: &str, label: &str) -> ApiResult<bool> {
        let mut label = Some(label.to_string());
        check_trimmed(&mut label, 200, 0)?;
        let (id, label) = (id.to_string(), label.unwrap_or_default());
        self.run(move |shared| async move { Ok(shared.history.set_label(&id, &label).await?) })
            .await
    }

    /// `DELETE /api/session-history/:id`.
    pub async fn delete_session_log(&self, id: &str) -> ApiResult<bool> {
        let id = id.to_string();
        self.run(move |shared| async move { Ok(shared.history.delete_session(&id).await?) })
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_slugs_match_the_typescript() {
        assert_eq!(export_slug("Prod: web-01 (eu)"), "prod-web-01-eu");
        assert_eq!(export_slug("--"), "session");
        assert_eq!(export_slug(&"a".repeat(60)), "a".repeat(48));
        assert_eq!(export_slug("Größe"), "gr-e");
        // Cut after the dashes are placed, as `slice(0, 48)` does.
        assert_eq!(
            export_slug(&format!("{} b", "a".repeat(47))),
            format!("{}-", "a".repeat(47))
        );
    }
}
