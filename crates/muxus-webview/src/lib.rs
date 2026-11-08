//! Saved web pages in their own browser windows (`electron/src/web-windows.ts`).
//!
//! A page window is a separate process: the `muxus` binary started with
//! [`protocol::WEB_WINDOW_ARG`], running its own tao event loop and a wry webview (WebKitGTK on
//! Linux, WKWebView on macOS, WebView2 on Windows). That keeps the browser engine's main loop
//! (GTK's, on Linux) away from GPUI's, and a crashing page away from the app. The app drives
//! its windows through [`WebWindows`]: one window per saved host, focused when opened again;
//! each keeps its own cookie jar and storage, and with an SSH gateway sends every request
//! (name lookups included) through the host's SOCKS5 proxy, which stops when the window
//! closes.
//!
//! The same window process also shows a running lab's topology with the containerlab viewer
//! ([`topology`]).
//!
//! The window itself needs the `webview` feature. Without it [`AVAILABLE`] is false and the
//! app opens pages the way the browser build of Muxus does (and draws topologies itself).

mod manager;
pub mod protocol;
pub mod request;
pub mod topology;
#[cfg(feature = "webview")]
mod window;

use std::ffi::OsStr;
use std::io::{BufRead, BufReader, Write};

pub use manager::{ClosedWebWindow, OpenedNode, WebWindows};
pub use protocol::{WEB_WINDOW_ARG, WebWindowCommand, WebWindowEvent, WebWindowLaunch};
pub use request::{
    WebWindowRequest, WebWindowResult, is_window_url, parse_web_window_request, proxy_port,
    proxy_url, refused_forward_id,
};
pub use topology::{TopologyNode, TopologyTheme, TopologyWindowRequest, WindowPlacement};

/// Whether this build shows pages in windows of its own.
pub const AVAILABLE: bool = cfg!(feature = "webview");

/// Tell the app something; a window whose app is gone has no one to tell.
fn report(event: &WebWindowEvent) {
    let mut stdout = std::io::stdout().lock();
    let _ = stdout.write_all(protocol::line(event).as_bytes());
    let _ = stdout.flush();
}

/// Run as a page window when the command line asks for one (`muxus --web-window`), before
/// anything else of the app starts. Returns the exit code, or `None` for a normal start.
pub fn run_if_requested() -> Option<i32> {
    let first = std::env::args_os().nth(1)?;
    if first != OsStr::new(WEB_WINDOW_ARG) {
        return None;
    }
    let mut input = BufReader::new(std::io::stdin());
    let mut text = String::new();
    let launch = match input.read_line(&mut text) {
        Ok(read) if read > 0 => serde_json::from_str::<WebWindowLaunch>(&text).ok(),
        _ => None,
    };
    let Some(launch) = launch else {
        report(&WebWindowEvent::Error {
            message: "The page window was started without a page.".into(),
        });
        return Some(2);
    };
    Some(run(launch, input))
}

#[cfg(feature = "webview")]
fn run(launch: WebWindowLaunch, commands: impl BufRead + Send + 'static) -> i32 {
    window::run(launch, commands)
}

#[cfg(not(feature = "webview"))]
fn run(_launch: WebWindowLaunch, _commands: impl BufRead + Send + 'static) -> i32 {
    report(&WebWindowEvent::Error {
        message: "This build of Muxus cannot show web pages in windows of its own.".into(),
    });
    2
}
