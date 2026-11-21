//! The shared sessions links as the backend last listed them, shared by every view that
//! shows one, and the checks that keep their hosts up to date (`api/shared-sessions.ts`,
//! `components/SharedSessionsSync.tsx`).

use std::collections::{BTreeSet, HashMap};
use std::time::Duration;

use muxus_core::api::{SharedSessionApplyResult, SharedSessionSource};
use muxus_core::folder_paths::sidebar_folder_key;
use muxus_session::Backend;
use muxus_ui::gpui_kit::{App, AsyncApp, Global, Task};
use muxus_ui::theme::Status;

use super::logic::{refresh_failed_message, refreshed_message, shared_session_changes};
use crate::features::launcher::kit::{show_error_toast, show_toast};

/// How often the app looks for links that are due.
const CHECK_INTERVAL: Duration = Duration::from_secs(60);

/// What `GET /api/shared-sessions` last answered, and what is going on with the links.
#[derive(Default)]
pub struct SharedSessions {
    /// `None` until the backend has answered.
    pub sources: Option<Vec<SharedSessionSource>>,
    /// Links being read right now, for the rows that show it.
    reading: BTreeSet<String>,
    /// Counts the checks and removals that may have changed hosts: a window that shows
    /// hosts reads them again when it moves.
    pub applied: u64,
}

impl Global for SharedSessions {}

/// The links as last listed; none until the backend has answered.
pub fn listed(cx: &App) -> &[SharedSessionSource] {
    cx.try_global::<SharedSessions>()
        .and_then(|state| state.sources.as_deref())
        .unwrap_or_default()
}

/// Whether the backend has listed the links yet.
pub fn loaded(cx: &App) -> bool {
    cx.try_global::<SharedSessions>()
        .is_some_and(|state| state.sources.is_some())
}

pub fn is_reading(id: &str, cx: &App) -> bool {
    cx.try_global::<SharedSessions>()
        .is_some_and(|state| state.reading.contains(id))
}

/// The link whose hosts live in the folder with this sidebar key, when one does.
pub fn link_for_folder<'a>(folder_key: &str, cx: &'a App) -> Option<&'a SharedSessionSource> {
    listed(cx)
        .iter()
        .find(|source| sidebar_folder_key(&source.folder) == folder_key)
}

/// The name of the link with this ID, for a host it keeps up to date.
pub fn link_name<'a>(id: &str, cx: &'a App) -> Option<&'a str> {
    listed(cx)
        .iter()
        .find(|source| source.id == id)
        .map(|source| source.name.as_str())
}

/// How many times hosts may have changed through a link.
pub fn applied(cx: &App) -> u64 {
    cx.try_global::<SharedSessions>()
        .map_or(0, |state| state.applied)
}

async fn list(backend: &Backend, cx: &mut AsyncApp) {
    if let Ok(response) = backend.shared_session_sources().await {
        cx.update(|cx| cx.default_global::<SharedSessions>().sources = Some(response.sources));
    }
}

/// Ask the backend for the links again; views observing [`SharedSessions`] redraw when the
/// answer is in.
pub fn refresh(backend: &Backend, cx: &mut App) {
    let backend = backend.clone();
    cx.spawn(async move |cx| list(&backend, cx).await).detach();
}

/// Read a link and make its hosts match, then show the result everywhere. `Ok(None)` when
/// the link was not due.
pub fn check(
    backend: &Backend,
    id: &str,
    force: bool,
    cx: &mut App,
) -> Task<Result<Option<SharedSessionApplyResult>, String>> {
    let backend = backend.clone();
    let id = id.to_owned();
    cx.default_global::<SharedSessions>()
        .reading
        .insert(id.clone());
    cx.spawn(async move |cx| {
        let result = backend.sync_shared_sessions(&id, force).await;
        // A failed check changes the link's status too.
        list(&backend, cx).await;
        cx.update(|cx| {
            let state = cx.default_global::<SharedSessions>();
            state.reading.remove(&id);
            state.applied += 1;
        });
        result.map_err(|error| error.message)
    })
}

/// Read a link because the user asked to, and say what came of it.
pub fn refresh_now(backend: &Backend, source: &SharedSessionSource, cx: &mut App) {
    let name = source.name.clone();
    let check = check(backend, &source.id, true, cx);
    cx.spawn(async move |cx| {
        let result = check.await;
        cx.update(|cx| match result {
            Ok(result) => show_toast(
                Status::Success,
                refreshed_message(&name, result.as_ref()),
                cx,
            ),
            Err(error) => show_error_toast(refresh_failed_message(&name, &error), cx),
        });
    })
    .detach();
}

/// Stop following a link; its hosts go with it unless they are kept.
pub fn stop_following(backend: &Backend, id: &str, keep_hosts: bool, cx: &mut App) {
    let backend = backend.clone();
    let id = id.to_owned();
    cx.spawn(async move |cx| {
        let result = backend.delete_shared_session_source(&id, keep_hosts).await;
        list(&backend, cx).await;
        cx.update(|cx| {
            cx.default_global::<SharedSessions>().applied += 1;
            if let Err(error) = result {
                show_error_toast(error.message, cx);
            }
        });
    })
    .detach();
}

/// The links that are due now and were not tried within the last interval. `attempted`
/// also keeps a link whose check cannot even be claimed from being retried in a loop.
fn due_now(
    sources: &[SharedSessionSource],
    attempted: &mut HashMap<String, i64>,
    now_ms: i64,
) -> Vec<String> {
    let interval = CHECK_INTERVAL.as_millis() as i64;
    let mut due = Vec::new();
    for source in sources {
        let last = attempted.get(&source.id).copied().unwrap_or(0);
        if source.due(now_ms) && now_ms - last >= interval {
            attempted.insert(source.id.clone(), now_ms);
            due.push(source.id.clone());
        }
    }
    due
}

/// Keep the hosts of shared session links up to date: at startup and while the app runs,
/// every link whose interval has passed is read again. Failures stay quiet here and show
/// with the link in Settings and on its folder; changed hosts are announced, since they
/// appear and disappear in the sidebar.
pub fn start(backend: &Backend, cx: &mut App) {
    let backend = backend.clone();
    cx.spawn(async move |cx| {
        let mut attempted: HashMap<String, i64> = HashMap::new();
        list(&backend, cx).await;
        loop {
            let due = cx.update(|cx| {
                due_now(
                    listed(cx),
                    &mut attempted,
                    chrono::Utc::now().timestamp_millis(),
                )
            });
            for id in due {
                let check = cx.update(|cx| check(&backend, &id, false, cx));
                cx.spawn(async move |cx| {
                    let Ok(Some(result)) = check.await else {
                        return;
                    };
                    let changes = shared_session_changes(&result);
                    if !changes.is_empty() {
                        let message = format!("{}: {changes}.", result.source.name);
                        cx.update(|cx| show_toast(Status::Info, message, cx));
                    }
                })
                .detach();
            }
            cx.background_executor().timer(CHECK_INTERVAL).await;
        }
    })
    .detach();
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(id: &str, minutes: u32, checked: Option<&str>) -> SharedSessionSource {
        SharedSessionSource {
            id: id.into(),
            name: id.into(),
            url: "https://portal.example.net/list".into(),
            folder: id.into(),
            refresh_minutes: minutes,
            host_count: 0,
            last_checked_at: checked.map(str::to_owned),
            last_synced_at: None,
            last_error: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn picks_the_links_that_are_due_and_does_not_hammer_one_that_stays_due() {
        let now = muxus_core::js::parse_timestamp("2026-11-20T10:00:00.000Z")
            .unwrap()
            .timestamp_millis();
        let sources = [
            link("never-read", 60, None),
            link("fresh", 60, Some("2026-11-20T09:30:00.000Z")),
            link("stale", 60, Some("2026-11-20T08:59:00.000Z")),
            link("on-request", 0, None),
        ];
        let mut attempted = HashMap::new();

        assert_eq!(
            due_now(&sources, &mut attempted, now),
            ["never-read", "stale"]
        );
        // Still due half a minute later (the backend is away, say): left alone.
        assert!(due_now(&sources, &mut attempted, now + 30_000).is_empty());
        // After a whole interval they are tried again.
        assert_eq!(
            due_now(&sources, &mut attempted, now + 60_000),
            ["never-read", "stale"]
        );
        // A link that became due in between is picked up on its own.
        assert_eq!(
            due_now(&sources, &mut attempted, now + 31 * 60_000),
            ["never-read", "fresh", "stale"]
        );
    }
}
