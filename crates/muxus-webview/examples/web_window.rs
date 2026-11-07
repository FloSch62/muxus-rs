//! Open a page window the way the app does, without the app:
//!
//! ```bash
//! cargo run -p muxus-webview --features webview --example web_window -- \
//!     https://example.com [socks-port] [seconds]
//! ```
//!
//! The example is its own window process (`--web-window`). It opens the page through the
//! SOCKS5 proxy on `127.0.0.1:<socks-port>` when one is given, keeps each page's data in a
//! scratch folder, closes the window after `seconds` when given, and prints how it closed.

use std::time::{Duration, Instant};

use muxus_webview::{WebWindowResult, WebWindows, proxy_url};

fn main() {
    if let Some(code) = muxus_webview::run_if_requested() {
        std::process::exit(code);
    }
    let mut args = std::env::args().skip(1);
    let url = args.next().unwrap_or_else(|| "https://example.com".into());
    let proxy: Option<u16> = args
        .next()
        .and_then(|port| port.parse().ok())
        .filter(|p| *p > 0);
    let mut seconds: Option<u64> = args.next().and_then(|s| s.parse().ok());
    let data = std::env::temp_dir().join("muxus-web-window-example");
    let windows = WebWindows::for_current_exe(&data).expect("the example's own path");
    let mut request = serde_json::json!({ "key": "example", "url": url, "title": "Example" });
    if let Some(port) = proxy {
        request["proxy"] = proxy_url(port).into();
        request["forwardId"] = "example-forward".into();
    }
    match windows.open(&request) {
        Ok(WebWindowResult::Opened) => println!("opened {url}"),
        other => {
            eprintln!("not opened: {other:?}");
            std::process::exit(1);
        }
    }
    // Opening it again only focuses it.
    println!("reopen: {:?}", windows.open(&request));
    let closed = windows.closed();
    let started = Instant::now();
    loop {
        if let Ok(window) = closed.try_recv() {
            println!("closed: {window:?}");
            break;
        }
        if seconds.is_some_and(|limit| started.elapsed() >= Duration::from_secs(limit)) {
            println!("closing after {}s", started.elapsed().as_secs());
            windows.close_all();
            seconds.take();
        }
        std::thread::sleep(Duration::from_millis(100));
        if started.elapsed() > Duration::from_secs(600) {
            break;
        }
    }
}
