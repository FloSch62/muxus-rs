//! Opening saved web pages from a window (`openWebHost` in `client/src/web-hosts.ts`, with the
//! Electron main process's `registerWebWindows`): one browser window per page, shared by every
//! app window; the gateway's sign-in questions go through the window's prompt dialogs.

use std::collections::{HashMap, HashSet};
use std::future::Future;
use std::time::Instant;

use muxus_app::features::forwarding::start::{DialAnswer, DialPrompt};
use muxus_app::features::web_hosts::{
    PageProxy, browser_proxy_notice, forward_running, refused_message, start_proxy, web_page,
    window_request,
};
use muxus_app::hosts::ManagedHost;
use muxus_app::hosts::saved::saved_host_display_name;
use muxus_app::state::session_actions::connect_managed_host;
use muxus_core::api::SavedHostProfile;
use muxus_core::protocol::SshGateway;
use muxus_session::Backend;
use muxus_ui::gpui_kit::{App, AsyncApp, Context, Global, WeakEntity};
use muxus_ui::theme::Status;
use muxus_webview::{WebWindowResult, WebWindows};

use crate::ui::prompts::{PromptReply, SessionPrompt};
use crate::ui::shell::AppShell;

/// The app's page windows, and the pages being opened.
struct WebPages {
    /// `None` when this build (or this system) has no page windows.
    windows: Option<WebWindows>,
    /// Hosts being opened right now, so a double click does not dial twice.
    opening: HashSet<String>,
    /// Without page windows: proxies started for pages, reused while they run.
    browser_proxies: HashMap<String, PageProxy>,
}

impl Global for WebPages {}

/// Set up page windows once at startup: each host's cookies live under
/// `<user data>/web-pages/<host id>`, a closing window stops its proxy, and the windows close
/// with the app.
pub fn install(backend: &Backend, cx: &mut App) {
    let windows = if muxus_webview::AVAILABLE {
        WebWindows::for_current_exe(backend.paths().user_data.join("web-pages")).ok()
    } else {
        None
    };
    if let Some(windows) = &windows {
        let closed = windows.closed();
        let backend = backend.clone();
        cx.spawn(async move |cx| {
            while let Ok(window) = closed.recv().await {
                if let Some(forward_id) = &window.forward_id {
                    let _ = backend.stop_forward(forward_id).await;
                }
                if let Some(error) = window.error {
                    cx.update(|cx| {
                        crate::ui::features::toast(Status::Error, error.into(), cx);
                    });
                }
            }
        })
        .detach();
        let open = windows.clone();
        cx.on_app_quit(move |_| {
            open.close_all();
            async {}
        })
        .detach();
    }
    cx.set_global(WebPages {
        windows,
        opening: HashSet::new(),
        browser_proxies: HashMap::new(),
    });
}

/// What is left to do in the window once a page is on its way.
enum Opened {
    Done,
    /// No page window: the system browser shows the page.
    InBrowser(String),
    /// No page window: tell where the page's proxy runs.
    Notice(String),
}

impl AppShell {
    /// `connectSavedHost`: a web page opens in its own browser window, never in a tab;
    /// everything else connects in a tab (the tab's id, empty for a page).
    pub fn connect_host(
        &mut self,
        host: &ManagedHost,
        replace: Option<&str>,
        cx: &mut Context<Self>,
    ) -> String {
        if let ManagedHost::Profile(entry) = host
            && web_page(entry).is_some()
        {
            self.open_web_host((**entry).clone(), cx);
            return String::new();
        }
        connect_managed_host(
            &mut self.tabs,
            &mut self.launch_guard,
            host,
            replace,
            Instant::now(),
        )
    }

    /// Open the web pages among `hosts` (`launchManagedHostGroup`): pages open in their own
    /// windows next to the tabs. Returns the rest.
    pub fn open_web_hosts_of(
        &mut self,
        hosts: &[ManagedHost],
        cx: &mut Context<Self>,
    ) -> Vec<ManagedHost> {
        let mut rest = Vec::new();
        for host in hosts {
            match host {
                ManagedHost::Profile(entry) if web_page(entry).is_some() => {
                    self.open_web_host((**entry).clone(), cx)
                }
                _ => rest.push(host.clone()),
            }
        }
        rest
    }

    /// `openWebHost`.
    pub fn open_web_host(&mut self, host: SavedHostProfile, cx: &mut Context<Self>) {
        let Some((url, gateway)) =
            web_page(&host).map(|(url, gateway)| (url.to_owned(), gateway.cloned()))
        else {
            return;
        };
        let Some(backend) = self.data.backend() else {
            self.show_toast(Status::Info, "Web pages need the Muxus backend.", cx);
            return;
        };
        let Some(pages) = cx.try_global::<WebPages>() else {
            return;
        };
        if pages.opening.contains(&host.id) {
            return;
        }
        let windows = pages.windows.clone();
        let reuse = pages.browser_proxies.get(&host.id).cloned();
        cx.global_mut::<WebPages>().opening.insert(host.id.clone());
        let key = format!("web:{}", host.id);
        let ask = window_asker(cx.entity().downgrade(), cx.to_async(), key.clone());
        cx.spawn(async move |weak, cx| {
            let result = match &windows {
                Some(windows) => open_in_window(&backend, windows, &host, gateway.as_ref(), ask)
                    .await
                    .map(|()| Opened::Done),
                None => match gateway {
                    None => Ok(Opened::InBrowser(url.clone())),
                    Some(gateway) => {
                        let proxy = match reuse {
                            Some(proxy) if forward_running(&backend, &proxy.forward_id).await => {
                                Ok(proxy)
                            }
                            _ => start_proxy(&backend, &gateway, ask).await,
                        };
                        proxy.map(|proxy| {
                            let notice = browser_proxy_notice(
                                saved_host_display_name(&host),
                                proxy.port,
                                &url,
                            );
                            cx.update(|cx| {
                                cx.global_mut::<WebPages>()
                                    .browser_proxies
                                    .insert(host.id.clone(), proxy);
                            });
                            Opened::Notice(notice)
                        })
                    }
                },
            };
            cx.update(|cx| {
                cx.global_mut::<WebPages>().opening.remove(&host.id);
            });
            let _ = weak.update(cx, |this, cx| {
                // A dial that gave up takes its waiting questions with it.
                this.close_prompts_of(&key, cx);
                match result {
                    Ok(Opened::Done) => {}
                    Ok(Opened::InBrowser(url)) => cx.open_url(&url),
                    Ok(Opened::Notice(notice)) => this.show_toast(Status::Info, notice, cx),
                    Err(message) => this.show_toast(Status::Error, message, cx),
                }
            });
        })
        .detach();
    }
}

/// The gateway's sign-in and host-key questions, asked in this window under `key`.
fn window_asker(
    shell: WeakEntity<AppShell>,
    cx: AsyncApp,
    key: String,
) -> impl FnMut(DialPrompt) -> std::pin::Pin<Box<dyn Future<Output = DialAnswer>>> {
    move |prompt| {
        let (reply, answer) = async_channel::bounded::<DialAnswer>(1);
        let key = key.clone();
        let prompt = match prompt {
            DialPrompt::Auth(info) => SessionPrompt::Auth(info),
            DialPrompt::HostKey(challenge) => SessionPrompt::HostKey(challenge),
        };
        let mut cx = cx.clone();
        let shell = shell.clone();
        Box::pin(async move {
            let _ = shell.update(&mut cx, |shell, cx| {
                shell.queue_prompt(key, prompt, Some(PromptReply::Tunnel(reply)), cx)
            });
            // A question taken back (the window closed) cancels the dial.
            answer.recv().await.unwrap_or(DialAnswer::Auth(None))
        })
    }
}

/// `openWebPage` with page windows: focus the open one, else start the proxy (with a gateway)
/// and open the window, which then owns the proxy.
async fn open_in_window<A, F>(
    backend: &Backend,
    windows: &WebWindows,
    host: &SavedHostProfile,
    gateway: Option<&SshGateway>,
    ask: A,
) -> Result<(), String>
where
    A: FnMut(DialPrompt) -> F,
    F: Future<Output = DialAnswer>,
{
    // An open window comes to the front; nothing is dialed again.
    if windows.focus(&host.id) {
        return Ok(());
    }
    let proxy = match gateway {
        Some(gateway) => Some(start_proxy(backend, gateway, ask).await?),
        None => None,
    };
    let Some(request) = window_request(host, proxy.as_ref()) else {
        return Ok(());
    };
    let result = windows.open(&request);
    // The window owns the proxy only when it opened with it.
    if !matches!(result, Ok(WebWindowResult::Opened))
        && let Some(proxy) = &proxy
    {
        let _ = backend.stop_forward(&proxy.forward_id).await;
    }
    match result {
        Ok(WebWindowResult::Opened | WebWindowResult::Focused) => Ok(()),
        // The window refused the request and the proxy is stopped.
        Ok(WebWindowResult::Invalid) => Err(refused_message(host)),
        Err(message) => Err(message),
    }
}
