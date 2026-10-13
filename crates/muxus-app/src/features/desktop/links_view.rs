//! Settings → Behavior → Links (`components/LinkHandlerSettings.tsx`): make Muxus the system
//! handler for ssh:// and telnet:// links. Each row shows the current handler, since the
//! choice changes a system default. The state is read again whenever the settings window is
//! focused: another program or the system settings may take the links back meanwhile.
//!
//! The view uses the settings layout ([`group`], [`row`], [`status_text`]) and the muxus-ui
//! [`Button`]; [`links_group`] is what the Behavior section embeds.

use muxus_core::api::{LinkHandlerState, LinkHandlerStatus};
use muxus_core::connection_links::ConnectionLinkScheme;
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext, Context, Entity, Global, IntoElement,
    ParentElement, Render, Styled, Subscription, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::Button;

use crate::features::launcher::kit::show_toast;
use crate::features::settings::view::kit::{group, row, secondary_text, status_text};

const SCHEMES: [(ConnectionLinkScheme, &str); 2] = [
    (ConnectionLinkScheme::Ssh, "SSH links"),
    (ConnectionLinkScheme::Telnet, "Telnet links"),
];

pub const LINKS_DESCRIPTION: &str = "Open links such as ssh://admin@10.0.0.1:2222 from a browser, wiki or monitoring dashboard as a tab in Muxus. Choosing Muxus changes a system-wide default; pick another program in the system settings to undo it.";

/// A row's description: its tone (none for plain text) and words (`handlerStatus`).
pub fn handler_status(
    scheme: ConnectionLinkScheme,
    state: Option<&LinkHandlerState>,
) -> (Option<Status>, String) {
    let Some(state) = state else {
        return (None, "Checking which program opens them…".into());
    };
    if let Some(unavailable) = &state.unavailable {
        return (Some(Status::Warning), unavailable.clone());
    }
    let status = status_for(state, scheme);
    let name = scheme.as_str();
    if status.is_default {
        return (
            Some(Status::Success),
            format!("Muxus opens {name}:// links."),
        );
    }
    (
        Some(Status::Info),
        match &status.current_handler {
            Some(handler) => format!("{handler} opens {name}:// links now."),
            None => format!("No program is set to open {name}:// links."),
        },
    )
}

fn status_for(state: &LinkHandlerState, scheme: ConnectionLinkScheme) -> &LinkHandlerStatus {
    match scheme {
        ConnectionLinkScheme::Ssh => &state.ssh,
        ConnectionLinkScheme::Telnet => &state.telnet,
    }
}

/// Whether a row's button is off.
pub fn register_disabled(
    scheme: ConnectionLinkScheme,
    state: Option<&LinkHandlerState>,
    busy: Option<ConnectionLinkScheme>,
) -> bool {
    match state {
        None => true,
        Some(state) => {
            state.unavailable.is_some() || status_for(state, scheme).is_default || busy.is_some()
        }
    }
}

/// The toast after a registration: its tone and words.
pub fn registration_toast(
    scheme: ConnectionLinkScheme,
    result: &muxus_core::api::LinkHandlerRegistration,
) -> (Status, String) {
    if let Some(error) = &result.error {
        (Status::Error, error.clone())
    } else if result.opened_system_settings == Some(true) {
        (
            Status::Info,
            format!(
                "Choose Muxus for {} in the default apps settings that opened.",
                scheme.as_str().to_uppercase()
            ),
        )
    } else {
        (
            Status::Success,
            format!("Muxus now opens {}:// links.", scheme.as_str()),
        )
    }
}

/// The Links group.
pub struct LinkHandlerSettings {
    backend: Backend,
    window: AnyWindowHandle,
    state: Option<LinkHandlerState>,
    /// The platform has no registration: the group stays hidden.
    unsupported: bool,
    busy: Option<ConnectionLinkScheme>,
    _activation: Subscription,
}

impl LinkHandlerSettings {
    pub fn new(backend: Backend, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let activation = cx.observe_window_activation(window, |this, window, cx| {
            if window.is_window_active() {
                this.refresh(cx);
            }
        });
        let mut this = Self {
            backend,
            window: window.window_handle(),
            state: None,
            unsupported: false,
            busy: None,
            _activation: activation,
        };
        this.refresh(cx);
        this
    }

    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.link_handlers().await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(Some(state)) => this.state = Some(state),
                    Ok(None) => this.unsupported = true,
                    // Like a failed IPC call: keep what is shown.
                    Err(_) => {}
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn register(&mut self, scheme: ConnectionLinkScheme, cx: &mut Context<Self>) {
        self.busy = Some(scheme);
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.register_link_handler(scheme).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(Some(result)) => {
                        this.state = Some(result.state.clone());
                        let (tone, message) = registration_toast(scheme, &result);
                        show_toast(tone, message, cx);
                    }
                    Ok(None) => {}
                    Err(_) => show_toast(
                        Status::Error,
                        format!(
                            "Could not make Muxus the handler for {}:// links.",
                            scheme.as_str()
                        ),
                        cx,
                    ),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn scheme_row(
        &self,
        theme: &Theme,
        scheme: ConnectionLinkScheme,
        label: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (tone, text) = handler_status(scheme, self.state.as_ref());
        let setting = match tone {
            Some(tone) => row(theme, label).description(status_text(theme, tone, text)),
            None => row(theme, label).description_text(theme, text),
        };
        setting
            .control(
                Button::new(
                    format!("link-handler-{}", scheme.as_str()),
                    format!("Use Muxus for {}:// links", scheme.as_str()),
                )
                .outlined()
                .disabled(register_disabled(scheme, self.state.as_ref(), self.busy))
                .on_click(cx.listener(move |this, _, _, cx| this.register(scheme, cx))),
            )
            .render()
    }
}

impl Render for LinkHandlerSettings {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.unsupported {
            return div().into_any_element();
        }
        let theme = *cx.theme();
        let rows: Vec<AnyElement> = SCHEMES
            .iter()
            .map(|(scheme, label)| self.scheme_row(&theme, *scheme, label, cx))
            .collect();
        group("Links")
            .description(secondary_text(&theme, LINKS_DESCRIPTION))
            .rows(rows)
            .render(&theme)
    }
}

/// The Links group of the one settings window, kept across renders.
struct LinksSettingsSlot(Option<Entity<LinkHandlerSettings>>);

impl Global for LinksSettingsSlot {}

/// The Links group for the Behavior section of the settings window that is rendering.
pub fn links_group(backend: &Backend, window: &mut Window, cx: &mut App) -> AnyElement {
    let handle = window.window_handle();
    let existing = cx
        .try_global::<LinksSettingsSlot>()
        .and_then(|slot| slot.0.clone())
        .filter(|entity| entity.read(cx).window == handle);
    let entity = match existing {
        Some(entity) => entity,
        None => {
            let backend = backend.clone();
            let entity = cx.new(|cx| LinkHandlerSettings::new(backend, window, cx));
            cx.set_global(LinksSettingsSlot(Some(entity.clone())));
            entity
        }
    };
    div().w_full().child(entity).into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::LinkHandlerRegistration;

    fn state(ssh: LinkHandlerStatus, telnet: LinkHandlerStatus) -> LinkHandlerState {
        LinkHandlerState {
            unavailable: None,
            ssh,
            telnet,
        }
    }

    fn status(is_default: bool, handler: Option<&str>) -> LinkHandlerStatus {
        LinkHandlerStatus {
            is_default,
            current_handler: handler.map(str::to_string),
        }
    }

    #[test]
    fn describes_each_scheme_s_handler() {
        let ssh = ConnectionLinkScheme::Ssh;
        let telnet = ConnectionLinkScheme::Telnet;
        assert_eq!(
            handler_status(ssh, None),
            (None, "Checking which program opens them…".into())
        );
        let current = state(status(true, Some("Muxus")), status(false, Some("PuTTY")));
        assert_eq!(
            handler_status(ssh, Some(&current)),
            (Some(Status::Success), "Muxus opens ssh:// links.".into())
        );
        assert_eq!(
            handler_status(telnet, Some(&current)),
            (
                Some(Status::Info),
                "PuTTY opens telnet:// links now.".into()
            )
        );
        let nobody = state(status(false, None), status(false, None));
        assert_eq!(
            handler_status(telnet, Some(&nobody)).1,
            "No program is set to open telnet:// links."
        );
        let missing = LinkHandlerState {
            unavailable: Some("xdg-mime was not found.".into()),
            ..nobody.clone()
        };
        assert_eq!(
            handler_status(ssh, Some(&missing)),
            (Some(Status::Warning), "xdg-mime was not found.".into())
        );
        assert!(register_disabled(ssh, None, None));
        assert!(register_disabled(ssh, Some(&missing), None));
        assert!(register_disabled(ssh, Some(&current), None));
        assert!(!register_disabled(telnet, Some(&current), None));
        assert!(register_disabled(telnet, Some(&current), Some(ssh)));
    }

    #[test]
    fn reports_how_a_registration_went() {
        let base = state(status(true, Some("Muxus")), status(false, None));
        let ok = LinkHandlerRegistration {
            state: base.clone(),
            error: None,
            opened_system_settings: None,
        };
        assert_eq!(
            registration_toast(ConnectionLinkScheme::Ssh, &ok),
            (Status::Success, "Muxus now opens ssh:// links.".into())
        );
        let opened = LinkHandlerRegistration {
            opened_system_settings: Some(true),
            ..ok.clone()
        };
        assert_eq!(
            registration_toast(ConnectionLinkScheme::Telnet, &opened).1,
            "Choose Muxus for TELNET in the default apps settings that opened."
        );
        let failed = LinkHandlerRegistration {
            error: Some("The desktop did not accept Muxus.".into()),
            ..ok
        };
        assert_eq!(
            registration_toast(ConnectionLinkScheme::Ssh, &failed).0,
            Status::Error
        );
    }
}
