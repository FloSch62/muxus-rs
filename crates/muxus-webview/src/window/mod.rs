//! The page window process (`openWebWindow` in `electron/src/web-windows.ts`): a tao window
//! with a wry webview on the host's own data folder and, with a gateway, its SOCKS5 proxy.

#[cfg(target_os = "linux")]
mod linux;

use std::io::BufRead;
use std::rc::Rc;

use tao::dpi::LogicalSize;
use tao::event::{Event, WindowEvent};
use tao::event_loop::{ControlFlow, EventLoopBuilder};
use tao::window::{Window, WindowBuilder};
use wry::{
    NewWindowFeatures, NewWindowResponse, PermissionResponse, ProxyConfig, ProxyEndpoint,
    WebContext, WebViewBuilder,
};

use crate::protocol::{WebWindowCommand, WebWindowEvent, WebWindowLaunch};
use crate::report;
use crate::request::{WebWindowRequest, is_window_url, parse_web_window_request, proxy_port};

enum UserEvent {
    Command(WebWindowCommand),
}

fn fail(message: impl Into<String>) -> i32 {
    report(&WebWindowEvent::Error {
        message: message.into(),
    });
    1
}

/// `<host title> — <page title>`, or the host's title while the page has none.
fn window_title(host_title: &str, page_title: &str) -> String {
    if page_title.is_empty() {
        host_title.to_owned()
    } else {
        format!("{host_title} — {page_title}")
    }
}

/// The settings every page view gets, the host window's and those of pages it opens: it stays
/// on http(s), and pages never get the camera, microphone, location or notifications.
pub(super) fn guarded(builder: WebViewBuilder<'_>) -> WebViewBuilder<'_> {
    builder
        .with_navigation_handler(|url| is_window_url(&url))
        .with_permission_handler(|_| PermissionResponse::Deny)
        .with_devtools(false)
        .with_new_window_req_handler(open_page_window)
}

/// A page a page opens (target=_blank) gets a window of its own, in the host's session and on
/// its proxy, with the same rules, and closes with the host window.
fn open_page_window(url: String, features: NewWindowFeatures) -> NewWindowResponse {
    if !is_window_url(&url) {
        return NewWindowResponse::Deny;
    }
    #[cfg(target_os = "linux")]
    {
        match linux::open_page_window(&url, &features.opener.webview) {
            Some(webview) => NewWindowResponse::Create { webview },
            None => NewWindowResponse::Deny,
        }
    }
    #[cfg(not(target_os = "linux"))]
    {
        // WebView2 and WKWebView open it in the opener's environment, so it keeps the
        // host's cookies and proxy.
        let _ = features;
        NewWindowResponse::Allow
    }
}

/// Alt+Left/Right and Ctrl+[ / Ctrl+] (Cmd on macOS) go back and forward, F5 and Ctrl+R
/// reload. On Linux the window handles them itself (see `linux::attach`); elsewhere the page
/// does, since the webview keeps the keyboard.
#[cfg(not(target_os = "linux"))]
const SHORTCUTS: &str = r#"
window.addEventListener('keydown', (event) => {
  const mod = navigator.platform.startsWith('Mac') ? event.metaKey : event.ctrlKey;
  if ((event.altKey && event.key === 'ArrowLeft') || (mod && event.key === '[')) {
    history.back();
  } else if ((event.altKey && event.key === 'ArrowRight') || (mod && event.key === ']')) {
    history.forward();
  } else if (event.key === 'F5' || (mod && event.key.toLowerCase() === 'r')) {
    location.reload();
  } else {
    return;
  }
  event.preventDefault();
}, true);
"#;

/// Show the page and run until the window closes, the app says so, or the app goes away.
pub(crate) fn run(launch: WebWindowLaunch, mut commands: impl BufRead + Send + 'static) -> i32 {
    let Some(request) = parse_web_window_request(&launch.request) else {
        return fail("The page cannot be shown: it is not an http:// or https:// address.");
    };
    #[cfg(target_os = "linux")]
    if let Err(error) = gtk::init() {
        return fail(format!("Cannot open a window for the page: {error}"));
    }
    let event_loop = EventLoopBuilder::<UserEvent>::with_user_event().build();
    let proxy = event_loop.create_proxy();
    // The app's commands; its stdin ending (the app quit or crashed) closes the window too.
    std::thread::spawn(move || {
        let mut text = String::new();
        loop {
            text.clear();
            match commands.read_line(&mut text) {
                Ok(read) if read > 0 => {
                    if let Ok(command) = serde_json::from_str::<WebWindowCommand>(&text) {
                        let _ = proxy.send_event(UserEvent::Command(command));
                    }
                }
                _ => break,
            }
        }
        let _ = proxy.send_event(UserEvent::Command(WebWindowCommand::Close));
    });

    let window = match WindowBuilder::new()
        .with_title(&request.title)
        .with_inner_size(LogicalSize::new(1200., 800.))
        .build(&event_loop)
    {
        Ok(window) => Rc::new(window),
        Err(error) => return fail(format!("Cannot open a window for the page: {error}")),
    };
    // One cookie jar and storage per host, so a page keeps its logins and never shares them
    // with another page or with Muxus.
    let mut context = WebContext::new(Some(launch.data_directory.clone()));
    let webview = match build(&window, &mut context, &request) {
        Ok(webview) => webview,
        Err(error) => return fail(format!("Cannot show the page: {error}")),
    };
    report(&WebWindowEvent::Opened);

    event_loop.run(move |event, _, control_flow| {
        *control_flow = ControlFlow::Wait;
        // The view and its data folder live as long as the window.
        let _ = (&webview, &context);
        match event {
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
            _ => {}
        }
    })
}

/// The host window's view.
fn build(
    window: &Rc<Window>,
    context: &mut WebContext,
    request: &WebWindowRequest,
) -> wry::Result<wry::WebView> {
    let host_title = request.title.clone();
    let titled = window.clone();
    let mut builder = guarded(WebViewBuilder::new_with_web_context(context))
        .with_url(&request.url)
        .with_focused(true)
        .with_document_title_changed_handler(move |title| {
            titled.set_title(&window_title(&host_title, &title));
        });
    if let Some(port) = request.proxy.as_deref().and_then(proxy_port) {
        // Every request, name lookups included, goes through the SOCKS5 forward on the host's
        // SSH gateway; names only the gateway knows resolve there, and `localhost` is the
        // gateway itself.
        builder = builder.with_proxy_config(ProxyConfig::Socks5(ProxyEndpoint {
            host: "127.0.0.1".into(),
            port: port.to_string(),
        }));
    }
    #[cfg(target_os = "linux")]
    {
        use tao::platform::unix::WindowExtUnix;
        use wry::{WebViewBuilderExtUnix, WebViewExtUnix};
        gtk::prelude::GtkWindowExt::set_icon_name(window.gtk_window(), Some("muxus"));
        let container = window
            .default_vbox()
            .ok_or(wry::Error::UnsupportedWindowHandle)?;
        let webview = builder.build_gtk(container)?;
        linux::attach(&webview.webview());
        Ok(webview)
    }
    #[cfg(not(target_os = "linux"))]
    {
        builder
            .with_initialization_script_for_main_only(SHORTCUTS, true)
            .build(window.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::window_title;

    #[test]
    fn titles_the_window_after_the_host_and_its_page() {
        assert_eq!(window_title("Grafana", ""), "Grafana");
        assert_eq!(window_title("Grafana", "Home"), "Grafana — Home");
    }
}
