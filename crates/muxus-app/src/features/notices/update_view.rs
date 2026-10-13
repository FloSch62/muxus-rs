//! `components/UpdateNotification.tsx`: the snackbar that says a new
//! release is out. Installed desktop builds with an in-app updater show its
//! decisions (download, restart to update, Store updates); every other build
//! points to the GitHub release the backend's manifest check found.
//!
//! Dismissals live in `client-state.json` through the backend, so a dismissed
//! version stays quiet in both builds. "Don't notify me again" turns the
//! `notifyOnNewVersion` preference off through the backend and tells the
//! window ([`UpdateNoticeEvent::Muted`]) so its copy of the prefs follows.

use muxus_core::api::{DesktopUpdateState, UpdateCheckResult};
use muxus_session::Backend;
use muxus_ui::gpui_kit::{
    App, AppContext, Context, EventEmitter, IntoElement, ParentElement, Render, SharedString,
    Styled, Window, div,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Status, Theme, weight};
use muxus_ui::widgets::display::{AlertVariant, alert_ink};
use muxus_ui::widgets::link::Link;
use muxus_ui::widgets::{Alert, Button, ButtonColor};

use super::update_notice::{
    DISMISSED_DESKTOP_UPDATE_KEY, DISMISSED_UPDATE_KEY, DesktopUpdateAction, NoticeTone,
    desktop_update_actions, desktop_update_notice, is_in_app_updater, release_update_message,
};
use crate::features::settings::hooks::settings_hooks;

/// What the notification tells its window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum UpdateNoticeEvent {
    /// `notifyOnNewVersion` was switched off (and saved).
    Muted,
}

/// A release the manifest check found.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ReleaseNotice {
    current_version: String,
    latest_version: String,
    release_url: String,
}

pub struct UpdateNotification {
    backend: Backend,
    notify: bool,
    /// The dismissed in-app decision, once read (`undefined` until then).
    desktop_dismissed: Option<Option<String>>,
    desktop_read_started: bool,
    release: Option<ReleaseNotice>,
    release_check_started: bool,
}

impl EventEmitter<UpdateNoticeEvent> for UpdateNotification {}

impl UpdateNotification {
    /// `notify_on_new_version` is the window's current preference.
    pub fn new(backend: Backend, notify_on_new_version: bool) -> Self {
        Self {
            backend,
            notify: notify_on_new_version,
            desktop_dismissed: None,
            desktop_read_started: false,
            release: None,
            release_check_started: false,
        }
    }

    /// The window's prefs changed.
    pub fn set_notify_on_new_version(&mut self, notify: bool, cx: &mut Context<Self>) {
        if self.notify != notify {
            self.notify = notify;
            cx.notify();
        }
    }

    /// For the documentation scene: show a release notice without asking the
    /// manifest.
    pub fn show_release(
        &mut self,
        current_version: &str,
        latest_version: &str,
        release_url: &str,
        cx: &mut Context<Self>,
    ) {
        self.release_check_started = true;
        self.release = Some(ReleaseNotice {
            current_version: current_version.into(),
            latest_version: latest_version.into(),
            release_url: release_url.into(),
        });
        cx.notify();
    }

    fn desktop_state(cx: &App) -> Option<DesktopUpdateState> {
        settings_hooks(cx)
            .desktop_update
            .and_then(|hooks| (hooks.state)(cx))
    }

    fn start_desktop_read(&mut self, cx: &mut Context<Self>) {
        if self.desktop_read_started {
            return;
        }
        self.desktop_read_started = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let value = backend
                .client_state_item(DISMISSED_DESKTOP_UPDATE_KEY)
                .await
                .ok()
                .flatten();
            let _ = this.update(cx, |this, cx| {
                if this.desktop_dismissed.is_none() {
                    this.desktop_dismissed = Some(value);
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// One manifest check per run (the backend shares it between callers).
    fn start_release_check(&mut self, cx: &mut Context<Self>) {
        if self.release_check_started {
            return;
        }
        self.release_check_started = true;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let Ok(UpdateCheckResult::Available {
                current_version,
                latest_version,
                release_url,
                ..
            }) = backend.update_check(false).await
            else {
                return;
            };
            let dismissed = backend
                .client_state_item(DISMISSED_UPDATE_KEY)
                .await
                .ok()
                .flatten();
            if dismissed.as_deref() == Some(latest_version.as_str()) {
                return;
            }
            let _ = this.update(cx, |this, cx| {
                this.release = Some(ReleaseNotice {
                    current_version,
                    latest_version,
                    release_url,
                });
                cx.notify();
            });
        })
        .detach();
    }

    fn remember(&self, key: &'static str, value: String, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        // Dismissal is a nicety; a failed write is ignored.
        cx.background_spawn(async move {
            let _ = backend.set_client_state_item(key, Some(&value)).await;
        })
        .detach();
    }

    /// "Later" on an in-app decision.
    fn later_desktop(&mut self, key: String, persist: bool, cx: &mut Context<Self>) {
        if persist {
            self.remember(DISMISSED_DESKTOP_UPDATE_KEY, key.clone(), cx);
        }
        self.desktop_dismissed = Some(Some(key));
        cx.notify();
    }

    /// "Later" (or Download) on a release notice.
    fn later_release(&mut self, cx: &mut Context<Self>) {
        if let Some(release) = self.release.take() {
            self.remember(DISMISSED_UPDATE_KEY, release.latest_version, cx);
        }
        cx.notify();
    }

    /// "Don't notify me again".
    fn mute(&mut self, cx: &mut Context<Self>) {
        self.notify = false;
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(mut prefs) = backend.prefs().await {
                prefs.notify_on_new_version = false;
                let _ = backend.set_prefs(prefs).await;
            }
            let _ = this.update(cx, |_, cx| cx.emit(UpdateNoticeEvent::Muted));
        })
        .detach();
        cx.notify();
    }

    fn run_desktop_action(action: DesktopUpdateAction, window: &mut Window, cx: &mut App) {
        let Some(hooks) = settings_hooks(cx).desktop_update else {
            return;
        };
        match action {
            // The desktop shell installs a Store update through the same
            // download step it uses for its own packages.
            DesktopUpdateAction::InstallStore | DesktopUpdateAction::Download => {
                (hooks.download)(window, cx)
            }
            DesktopUpdateAction::OpenStore => (hooks.open_store)(window, cx),
            DesktopUpdateAction::Restart => (hooks.install)(window, cx),
        }
    }
}

/// The snackbar: a filled `Alert` centred 24px above the window's bottom edge,
/// the message and its buttons on one row, the mute link under them.
fn snackbar(
    tone: NoticeTone,
    message: SharedString,
    actions: Vec<Button>,
    on_mute: impl Fn(&mut Window, &mut App) + 'static,
    cx: &App,
) -> impl IntoElement {
    let theme = cx.theme();
    let status = tone_status(tone);
    let ink = alert_ink(theme, status, AlertVariant::Filled);
    let content = div()
        .flex()
        .flex_col()
        .child(
            div()
                .flex()
                .items_center()
                .gap(Theme::spacing(2.))
                .child(
                    label(message)
                        .variant(theme.typography.body2)
                        .weight(weight::MEDIUM)
                        .color(ink)
                        .no_truncate(),
                )
                // The buttons' `my: '-5px'` is lost to the row Stack's own
                // `margin: 0` (its spacing rule), so the row is button height.
                .child(div().flex().gap(Theme::spacing(0.5)).children(actions)),
        )
        .child(
            div().mt(Theme::spacing(0.5)).child(
                Link::new("update-mute", "Don\u{2019}t notify me again")
                    .opacity(0.85)
                    .on_click(move |_, window, cx| on_mute(window, cx)),
            ),
        );
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(Theme::spacing(3.))
        .flex()
        .justify_center()
        .child(Alert::new(status, "").filled().content(content))
}

fn inherit_button(id: impl Into<SharedString>, label: &'static str) -> Button {
    Button::new(muxus_ui::gpui_kit::ElementId::Name(id.into()), label).color(ButtonColor::Inherit)
}

impl Render for UpdateNotification {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let empty = || div().into_any_element();
        if !self.notify {
            return empty();
        }
        let state = Self::desktop_state(cx);
        let entity = cx.entity().downgrade();
        if is_in_app_updater(state.as_ref()) {
            let state = state.expect("checked above");
            self.start_desktop_read(cx);
            let Some(notice) = desktop_update_notice(Some(&state)) else {
                return empty();
            };
            let Some(dismissed) = &self.desktop_dismissed else {
                return empty();
            };
            if dismissed.as_deref() == Some(notice.key.as_str()) {
                return empty();
            }
            let mut actions = Vec::new();
            for action in desktop_update_actions(&state) {
                let (entity, key) = (entity.clone(), notice.key.clone());
                let persist = notice.persist_dismissal;
                actions.push(
                    inherit_button(format!("update-{}", action.label()), action.label()).on_click(
                        move |_, window, cx| {
                            if action.dismisses() {
                                let _ = entity.update(cx, |this, cx| {
                                    this.later_desktop(key.clone(), persist, cx)
                                });
                            }
                            Self::run_desktop_action(action, window, cx);
                        },
                    ),
                );
            }
            let (later, key) = (entity.clone(), notice.key.clone());
            let persist = notice.persist_dismissal;
            actions.push(
                inherit_button("update-later", "Later").on_click(move |_, _, cx| {
                    let _ =
                        later.update(cx, |this, cx| this.later_desktop(key.clone(), persist, cx));
                }),
            );
            let (mute, key) = (entity, notice.key.clone());
            return snackbar(
                notice.tone,
                notice.message.into(),
                actions,
                move |_, cx| {
                    // `onMuted={later}`: the decision is dismissed as well.
                    let _ = mute.update(cx, |this, cx| {
                        this.later_desktop(key.clone(), persist, cx);
                        this.mute(cx);
                    });
                },
                cx,
            )
            .into_any_element();
        }
        self.start_release_check(cx);
        let Some(release) = self.release.clone() else {
            return empty();
        };
        let (download, later, mute) = (entity.clone(), entity.clone(), entity);
        let url = release.release_url.clone();
        let actions = vec![
            inherit_button("update-download", "Download").on_click(move |_, _, cx| {
                cx.open_url(&url);
                let _ = download.update(cx, |this, cx| this.later_release(cx));
            }),
            inherit_button("update-later", "Later").on_click(move |_, _, cx| {
                let _ = later.update(cx, |this, cx| this.later_release(cx));
            }),
        ];
        snackbar(
            NoticeTone::Info,
            release_update_message(&release.latest_version, &release.current_version).into(),
            actions,
            move |_, cx| {
                // `onMuted={() => setUpdate(null)}`: hidden, not dismissed.
                let _ = mute.update(cx, |this, cx| {
                    this.release = None;
                    this.mute(cx);
                });
            },
            cx,
        )
        .into_any_element()
    }
}

/// Mount the notification for a window.
pub fn update_notification(
    backend: Backend,
    notify_on_new_version: bool,
    cx: &mut App,
) -> muxus_ui::gpui_kit::Entity<UpdateNotification> {
    cx.new(|_| UpdateNotification::new(backend, notify_on_new_version))
}

/// The tone as the toast host's status, for windows that show the notice as
/// an ordinary toast instead.
pub fn tone_status(tone: NoticeTone) -> Status {
    match tone {
        NoticeTone::Info => Status::Info,
        NoticeTone::Warning => Status::Warning,
    }
}
