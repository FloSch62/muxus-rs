//! The topology window (see [`crate::topology`]): a tao window with a wry view on the
//! `muxus-topology` scheme, which serves the dialog page (`assets/topology.html`), the lab as
//! `lab.json`, and the containerlab viewer build (`assets/clab-viewer`, as the TypeScript
//! client ships it under `/vendor/clab-viewer/`).

use std::borrow::Cow;
use std::io::BufRead;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tao::dpi::{LogicalSize, PhysicalPosition, PhysicalSize};
use tao::event::{Event, StartCause, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowBuilder};
use wry::http::{Response, StatusCode, header::CONTENT_TYPE};
use wry::{NewWindowResponse, PageLoadEvent, PermissionResponse, WebViewBuilder};

use super::{UserEvent, fail, read_commands};
use crate::protocol::{WebWindowCommand, WebWindowEvent};
use crate::report;
use crate::topology::{
    TopologyTheme, TopologyWindowRequest, WindowPlacement, is_valid_topology_request, node_to_open,
};

/// A window whose page has not said it finished loading by then shows anyway.
const SHOW_AT_THE_LATEST: Duration = Duration::from_secs(3);

/// The scheme the window's pages come from.
const SCHEME: &str = "muxus-topology";

/// Where the dialog page is: WebView2 maps a custom scheme onto `http://<scheme>.localhost`.
fn base_url() -> String {
    if cfg!(windows) {
        format!("http://{SCHEME}.localhost/")
    } else {
        format!("{SCHEME}://localhost/")
    }
}

/// The dialog page, with the two button icons in place.
fn dialog_page() -> String {
    include_str!("../../assets/topology.html")
        .replace(
            "<!--LoginOutlined-->",
            include_str!("../../../../assets/icons/LoginOutlined.svg"),
        )
        .replace(
            "<!--TerminalOutlined-->",
            include_str!("../../../../assets/icons/TerminalOutlined.svg"),
        )
}

const HTML: &str = "text/html; charset=utf-8";
const JAVASCRIPT: &str = "text/javascript; charset=utf-8";
const CSS: &str = "text/css; charset=utf-8";

macro_rules! viewer_assets {
    ($($path:literal => $kind:expr),* $(,)?) => {
        /// The containerlab viewer build, by path under the scheme's root.
        const VIEWER: &[(&str, &[u8], &str)] = &[
            $(($path, include_bytes!(concat!("../../assets/clab-viewer/", $path)), $kind)),*
        ];
    };
}

// `@containerlab/clab-viewer` 0.2.0's `dist`, less what the TypeScript client leaves out too
// (`clab-viewer-plugin.ts`: the web component, the bundler entry, type declarations and the
// size report).
viewer_assets!(
    "viewer.html" => HTML,
    "styles.css" => CSS,
    "assets/AnnotationHandles-xyhAle_3.js" => JAVASCRIPT,
    "assets/FreeShapeNode-CL8CkuAu.js" => JAVASCRIPT,
    "assets/FreeTextNode-BJB9f8BH.js" => JAVASCRIPT,
    "assets/GroupNode-DlybhYCK.js" => JAVASCRIPT,
    "assets/NodeRuntimeBadge-DhYvw9xv.js" => JAVASCRIPT,
    "assets/Popper-DjmgI9Ko.js" => JAVASCRIPT,
    "assets/TrafficChart-7KtZMo0Y.js" => JAVASCRIPT,
    "assets/TrafficRateNode-DGM69Czj.js" => JAVASCRIPT,
    "assets/createSvgIcon-hCvh0sZE.js" => JAVASCRIPT,
    "assets/jsx-runtime-BNakU3Ej.js" => JAVASCRIPT,
    "assets/markdownRenderer-CaI551bR.js" => JAVASCRIPT,
    "assets/mountViewer-QuEEYzuW.js" => JAVASCRIPT,
    "assets/rolldown-runtime-hePW80VL.js" => JAVASCRIPT,
    "assets/timeout-DVN2vp-7.js" => JAVASCRIPT,
    "assets/types-CblE_Dqm.js" => JAVASCRIPT,
    "assets/utils-QqGprQRV.js" => JAVASCRIPT,
    "assets/viewer-DOAHIrmF.js" => JAVASCRIPT,
);

/// What the scheme serves at `path`: the dialog page, the lab, or a file of the viewer.
fn serve(
    path: &str,
    lab: &Mutex<TopologyWindowRequest>,
) -> Option<(Cow<'static, [u8]>, &'static str)> {
    let path = path.trim_start_matches('/');
    match path {
        "" | "index.html" => Some((Cow::Owned(dialog_page().into_bytes()), HTML)),
        "lab.json" => {
            let lab = lab.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            Some((
                Cow::Owned(serde_json::to_vec(&*lab).unwrap_or_default()),
                "application/json",
            ))
        }
        _ => VIEWER
            .iter()
            .find(|(name, _, _)| *name == path)
            .map(|(_, bytes, kind)| (Cow::Borrowed(*bytes), *kind)),
    }
}

/// `openAllowedExternalUrl`: links out of the viewer (its documentation links) open in the
/// system browser, http(s) only.
fn open_in_browser(url: &str) {
    if !crate::is_window_url(url) {
        return;
    }
    #[cfg(target_os = "linux")]
    let command = std::process::Command::new("xdg-open").arg(url).spawn();
    #[cfg(target_os = "macos")]
    let command = std::process::Command::new("open").arg(url).spawn();
    #[cfg(windows)]
    let command = std::process::Command::new("rundll32")
        .args(["url.dll,FileProtocolHandler", url])
        .spawn();
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    let command: std::io::Result<std::process::Child> = Err(std::io::ErrorKind::Unsupported.into());
    if let Ok(mut child) = command {
        // Reap it once the opener has handed the address on.
        std::thread::spawn(move || child.wait());
    }
}

/// Put the page area at `placement`, with the frame and title bar around it.
fn place(window: &Window, placement: WindowPlacement) {
    let (Ok(outer), Ok(inner)) = (window.outer_position(), window.inner_position()) else {
        return;
    };
    window.set_outer_position(PhysicalPosition::new(
        placement.x - (inner.x - outer.x),
        placement.y - (inner.y - outer.y),
    ));
}

/// The page's call to follow a new theme.
fn theme_script(theme: &TopologyTheme) -> String {
    let theme = serde_json::to_string(theme).unwrap_or_else(|_| "null".into());
    format!("window.muxusTopology && window.muxusTopology.setTheme({theme});")
}

/// Show the lab and run until the window closes, a node is opened, the app says so, or the
/// app goes away.
pub(crate) fn run(request: TopologyWindowRequest, commands: impl BufRead + Send + 'static) -> i32 {
    if !is_valid_topology_request(&request) {
        return fail("The topology cannot be shown.");
    }
    #[cfg(target_os = "linux")]
    if let Err(error) = gtk::init() {
        return fail(format!("Cannot open a window for the topology: {error}"));
    }
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    read_commands(commands, proxy.clone());

    // Hidden until its page is drawn: the app's dialog stays up until then, and the window
    // then shows where the dialog was, rather than blank and wherever the system puts it.
    let mut builder = WindowBuilder::new()
        .with_title(&request.title)
        .with_visible(false);
    builder = match request.placement {
        Some(placement) => builder
            .with_position(PhysicalPosition::new(placement.x, placement.y))
            .with_inner_size(PhysicalSize::new(placement.width, placement.height)),
        None => builder.with_inner_size(LogicalSize::new(1100., 760.)),
    };
    let window = match builder.build(&event_loop) {
        Ok(window) => window,
        Err(error) => return fail(format!("Cannot open a window for the topology: {error}")),
    };
    if let Some(placement) = request.placement {
        place(&window, placement);
    }
    #[cfg(windows)]
    super::set_app_icon(&window);

    let lab = Arc::new(Mutex::new(request.clone()));
    let served = lab.clone();
    let base = base_url();
    let inside = base.clone();
    let closing = proxy.clone();
    let loaded = proxy.clone();
    let builder = WebViewBuilder::new()
        // The viewer keeps nothing between windows.
        .with_incognito(true)
        .with_devtools(false)
        .with_custom_protocol(SCHEME.into(), move |_, request| {
            let response = match serve(request.uri().path(), &served) {
                Some((body, kind)) => Response::builder().header(CONTENT_TYPE, kind).body(body),
                None => Response::builder()
                    .status(StatusCode::NOT_FOUND)
                    .body(Cow::Borrowed(&[][..])),
            };
            response.unwrap_or_else(|_| Response::new(Cow::Borrowed(&[][..])))
        })
        .with_navigation_handler(move |url| {
            if url.starts_with(&inside) || url == "about:blank" {
                return true;
            }
            open_in_browser(&url);
            false
        })
        .with_new_window_req_handler(|url, _| {
            open_in_browser(&url);
            NewWindowResponse::Deny
        })
        .with_permission_handler(|_| PermissionResponse::Deny)
        .with_on_page_load_handler(move |event, _| {
            if matches!(event, PageLoadEvent::Finished) {
                let _ = loaded.send_event(UserEvent::PageLoaded);
            }
        })
        .with_ipc_handler(move |message| {
            // "SSH as admin" / "Open shell": the app opens the node, and the dialog closes.
            if let Some(name) = node_to_open(&request, message.body()) {
                report(&WebWindowEvent::OpenNode { name });
                let _ = closing.send_event(UserEvent::Command(WebWindowCommand::Close));
            }
        })
        .with_url(format!("{base}index.html"));

    #[cfg(target_os = "linux")]
    let built = {
        use tao::platform::unix::WindowExtUnix;
        use wry::{WebViewBuilderExtUnix, WebViewExtUnix};
        gtk::prelude::GtkWindowExt::set_icon_name(window.gtk_window(), Some("muxus"));
        match window.default_vbox() {
            Some(container) => builder.build_gtk(container).inspect(|webview| {
                super::linux::without_context_menu(&webview.webview());
            }),
            None => Err(wry::Error::UnsupportedWindowHandle),
        }
    };
    #[cfg(not(target_os = "linux"))]
    let built = builder.build(&window);
    let webview = match built {
        Ok(webview) => webview,
        Err(error) => return fail(format!("Cannot show the topology: {error}")),
    };

    let show_by = Instant::now() + SHOW_AT_THE_LATEST;
    let mut shown = false;
    event_loop.run(move |event, _, control_flow| {
        *control_flow = if shown {
            ControlFlow::Wait
        } else {
            ControlFlow::WaitUntil(show_by)
        };
        match event {
            Event::UserEvent(UserEvent::PageLoaded)
            | Event::NewEvents(StartCause::ResumeTimeReached { .. })
                if !shown =>
            {
                shown = true;
                *control_flow = ControlFlow::Wait;
                window.set_visible(true);
                window.set_focus();
                // The app closes its dialog now.
                report(&WebWindowEvent::Opened);
            }
            Event::WindowEvent {
                event: WindowEvent::CloseRequested,
                ..
            }
            | Event::UserEvent(UserEvent::Command(WebWindowCommand::Close)) => {
                *control_flow = ControlFlow::Exit;
            }
            Event::UserEvent(UserEvent::Command(WebWindowCommand::Focus)) => {
                if window.is_minimized() {
                    window.set_minimized(false);
                }
                window.set_focus();
            }
            Event::UserEvent(UserEvent::Command(WebWindowCommand::Theme { theme })) => {
                let _ = webview.evaluate_script(&theme_script(&theme));
                lab.lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .theme = *theme;
            }
            _ => {}
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serves_every_file_of_the_viewer_build() {
        // Every file the build ships is listed (an update of the viewer cannot leave one out),
        // and nothing else of it is.
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/clab-viewer");
        let mut shipped = Vec::new();
        for entry in walk(&root) {
            let name = entry
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if name != "LICENSE" {
                shipped.push(name);
            }
        }
        shipped.sort();
        let mut listed: Vec<String> = VIEWER.iter().map(|(name, _, _)| name.to_string()).collect();
        listed.sort();
        assert_eq!(listed, shipped);
    }

    fn walk(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
        let mut files = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                files.extend(walk(&path));
            } else {
                files.push(path);
            }
        }
        files
    }

    #[test]
    fn serves_the_dialog_the_lab_and_the_viewer() {
        let lab = Mutex::new(crate::topology::tests::request());
        let (page, kind) = serve("/", &lab).unwrap();
        assert_eq!(kind, HTML);
        let page = String::from_utf8(page.into_owned()).unwrap();
        // The icons are in place, and the page frames the viewer with the app's origin.
        assert!(page.contains("<template id=\"icon-ssh\"><svg"));
        assert!(page.contains("viewer.html?"));
        let (json, kind) = serve("/lab.json", &lab).unwrap();
        assert_eq!(kind, "application/json");
        let back: TopologyWindowRequest = serde_json::from_slice(&json).unwrap();
        assert_eq!(back, crate::topology::tests::request());
        let (viewer, kind) = serve("/viewer.html", &lab).unwrap();
        assert_eq!(kind, HTML);
        assert!(String::from_utf8_lossy(&viewer).contains("./assets/viewer-"));
        assert_eq!(
            serve("/assets/viewer-DOAHIrmF.js", &lab).unwrap().1,
            JAVASCRIPT
        );
        assert!(serve("/../Cargo.toml", &lab).is_none());
        assert!(serve("/index.d.ts", &lab).is_none());
    }

    #[test]
    fn hands_a_new_theme_to_the_page() {
        let script = theme_script(&crate::topology::tests::theme());
        assert!(script.starts_with("window.muxusTopology && window.muxusTopology.setTheme({"));
        assert!(script.contains("\"mode\":\"dark\""));
    }
}
