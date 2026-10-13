//! What the file browser shares across views and windows: the backend, the
//! app's notifications, listings by connection (React Query's `sftp-list`
//! cache), uploads dropped onto terminals (`state/sftp-uploads.ts`) and the
//! remote files opened with local programs (`state/local-copies.ts`).
//!
//! The app installs these once with [`init`]; every view reads them through
//! [`services`].

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use muxus_core::api::SftpListResponse;
use muxus_session::{ApiError, Backend};
use muxus_ssh::sftp::CancelSignal;
use muxus_ui::gpui_kit::{App, AppContext, Context, Entity, Global, SharedString};
use muxus_ui::theme::Status;

use super::transfer::{TransferPhase, TransferProgressState};

/// Shows a notification (`showToast`): severity, message and optional details.
pub type Notify = Rc<dyn Fn(Status, SharedString, Option<SharedString>, &mut App)>;

/// The shared services, a GPUI global.
#[derive(Clone)]
pub struct SftpServices {
    pub backend: Backend,
    pub notify: Notify,
    pub listings: Entity<SftpListings>,
    pub uploads: Entity<SftpUploads>,
    pub local_copies: Entity<super::local_copies::LocalCopies>,
}

impl Global for SftpServices {}

/// Install the services for the whole app. Call once, after `muxus_ui::init`.
pub fn init(backend: Backend, notify: Notify, cx: &mut App) {
    let listings = cx.new(|_| SftpListings::default());
    let uploads = cx.new(|_| SftpUploads::default());
    let local_copies = cx.new(|_| super::local_copies::LocalCopies::default());
    cx.set_global(SftpServices {
        backend,
        notify,
        listings,
        uploads,
        local_copies,
    });
}

/// The installed services.
pub fn services(cx: &App) -> SftpServices {
    cx.global::<SftpServices>().clone()
}

/// `showToast`.
pub fn notify(status: Status, message: impl Into<SharedString>, cx: &mut App) {
    let notify = cx.global::<SftpServices>().notify.clone();
    notify(status, message.into(), None, cx);
}

/// `errorDetails`: the status and body of a failed call, when they say more
/// than the message.
pub fn error_details(error: &ApiError) -> Option<String> {
    let mut parts = Vec::new();
    if error.status > 0 {
        parts.push(format!("HTTP {}", error.status));
    }
    if let Ok(body) = serde_json::to_string_pretty(&error.body()) {
        parts.push(body);
    }
    let text = parts.join("\n");
    (!text.is_empty() && text != error.message).then_some(text)
}

/// `showErrorToast`.
pub fn notify_error(error: &ApiError, cx: &mut App) {
    let notify = cx.global::<SftpServices>().notify.clone();
    notify(
        Status::Error,
        error.message.clone().into(),
        error_details(error).map(Into::into),
        cx,
    );
}

/// How long a listing counts as fresh (the client's `staleTime`).
pub const LISTING_STALE_AFTER: Duration = Duration::from_secs(15);

/// One fetched listing.
#[derive(Clone, Debug)]
pub struct CachedListing {
    pub response: SftpListResponse,
    pub fetched_at: Instant,
    /// The connection's refresh generation it was fetched in.
    pub generation: u64,
}

/// Listings by connection and requested path, and a refresh counter per
/// connection: `refresh` (`invalidateQueries(['sftp-list', connId])`) makes
/// every browser of the connection fetch again.
#[derive(Default)]
pub struct SftpListings {
    cache: HashMap<(String, String), CachedListing>,
    generations: HashMap<String, u64>,
}

impl SftpListings {
    pub fn generation(&self, conn_id: &str) -> u64 {
        self.generations.get(conn_id).copied().unwrap_or(0)
    }

    pub fn get(&self, conn_id: &str, path: &str) -> Option<&CachedListing> {
        self.cache.get(&(conn_id.to_string(), path.to_string()))
    }

    /// Cached, from this generation and younger than the stale time.
    pub fn is_fresh(&self, conn_id: &str, path: &str) -> bool {
        self.get(conn_id, path).is_some_and(|listing| {
            listing.generation == self.generation(conn_id)
                && listing.fetched_at.elapsed() < LISTING_STALE_AFTER
        })
    }

    pub fn store(
        &mut self,
        conn_id: &str,
        path: &str,
        response: SftpListResponse,
        generation: u64,
    ) {
        self.cache.insert(
            (conn_id.to_string(), path.to_string()),
            CachedListing {
                response,
                fetched_at: Instant::now(),
                generation,
            },
        );
    }

    /// Re-read every listing of the connection.
    pub fn refresh(&mut self, conn_id: &str, cx: &mut Context<Self>) {
        *self.generations.entry(conn_id.to_string()).or_default() += 1;
        cx.notify();
    }

    /// Forget a connection that closed.
    pub fn forget(&mut self, conn_id: &str) {
        self.cache.retain(|(conn, _), _| conn != conn_id);
    }
}

/// Files dropped onto an SSH terminal, on their way to the session's host
/// (`TerminalUpload`).
#[derive(Clone, Debug)]
pub struct TerminalUpload {
    /// Distinguishes uploads so a finished one clears only itself.
    pub id: u64,
    pub conn_id: String,
    /// The tab's title, to say where the files go.
    pub host: Option<String>,
    pub transfer: TransferProgressState,
    pub cancel: CancelSignal,
}

/// `useSftpUploadsStore`: at most one upload per connection, and how many
/// file browsers show each connection.
#[derive(Default)]
pub struct SftpUploads {
    uploads: HashMap<String, TerminalUpload>,
    browsers: HashMap<String, usize>,
}

impl SftpUploads {
    pub fn get(&self, conn_id: &str) -> Option<&TerminalUpload> {
        self.uploads.get(conn_id)
    }

    pub fn show(&mut self, upload: TerminalUpload) {
        self.uploads.insert(upload.conn_id.clone(), upload);
    }

    /// Clears the connection's upload if it is still `id`.
    pub fn clear(&mut self, conn_id: &str, id: u64) {
        if self
            .uploads
            .get(conn_id)
            .is_some_and(|upload| upload.id == id)
        {
            self.uploads.remove(conn_id);
        }
    }

    /// The bar's Cancel: shows the upload as stopping and signals it.
    pub fn cancel(&mut self, conn_id: &str) {
        if let Some(upload) = self.uploads.get_mut(conn_id) {
            upload.transfer.phase = TransferPhase::Cancelling;
            upload.transfer.bytes_per_second = 0.;
            upload.cancel.cancel();
        }
    }

    /// A file browser for the connection came on screen.
    pub fn attach_browser(&mut self, conn_id: &str) {
        *self.browsers.entry(conn_id.to_string()).or_default() += 1;
    }

    /// It went away again.
    pub fn detach_browser(&mut self, conn_id: &str) {
        if let Some(count) = self.browsers.get_mut(conn_id) {
            *count -= 1;
            if *count == 0 {
                self.browsers.remove(conn_id);
            }
        }
    }

    /// File browsers on screen for the connection; without one, the upload
    /// shows in a corner of the window instead.
    pub fn browsers(&self, conn_id: &str) -> usize {
        self.browsers.get(conn_id).copied().unwrap_or(0)
    }

    pub fn is_empty(&self) -> bool {
        self.uploads.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_file_browsers_showing_a_connection() {
        let mut store = SftpUploads::default();
        store.attach_browser("conn-1");
        store.attach_browser("conn-1");
        assert_eq!(store.browsers("conn-1"), 2);
        store.detach_browser("conn-1");
        assert_eq!(store.browsers("conn-1"), 1);
        store.detach_browser("conn-1");
        assert_eq!(store.browsers("conn-1"), 0);
        assert!(store.browsers.is_empty());
    }

    #[test]
    fn details_name_the_status_and_body() {
        let error = ApiError::new(404, "connection not found");
        let details = error_details(&error).unwrap();
        assert!(details.starts_with("HTTP 404\n{"));
        assert!(details.contains("\"message\": \"connection not found\""));
    }
}
