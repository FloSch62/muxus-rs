//! The connected password managers as the backend last listed them, shared by every view
//! that names one, and the way a field asks the window for the entry picker.

use std::rc::Rc;

use muxus_core::api::{ExternalPasswordRef, PasswordManagerSource, PasswordManagersResponse};
use muxus_session::Backend;
use muxus_ui::gpui_kit::{App, Global};

/// What `GET /api/password-managers` last answered; `None` until it has.
#[derive(Default)]
pub struct PasswordManagerList {
    pub response: Option<PasswordManagersResponse>,
}

impl Global for PasswordManagerList {}

/// The managers as last listed.
pub fn listed(cx: &App) -> Option<&PasswordManagersResponse> {
    cx.try_global::<PasswordManagerList>()
        .and_then(|list| list.response.as_ref())
}

pub fn listed_sources(cx: &App) -> Option<&[PasswordManagerSource]> {
    listed(cx).map(|response| response.sources.as_slice())
}

/// Ask the backend again. A manager is locked, unlocked and signed in to outside Muxus
/// too, so every view that shows one asks when it opens; views observing
/// [`PasswordManagerList`] redraw when the answer is in.
pub fn refresh(backend: &Backend, cx: &mut App) {
    let backend = backend.clone();
    cx.spawn(async move |cx| {
        if let Ok(response) = backend.password_managers_list().await {
            cx.update(|cx| {
                cx.set_global(PasswordManagerList {
                    response: Some(response),
                });
            });
        }
    })
    .detach();
}

/// What to do with the entry the picker hands back.
pub type EntryPicked = Box<dyn FnOnce(ExternalPasswordRef, &mut App)>;

/// A field's request for the entry picker.
pub struct PickRequest {
    /// The entry linked now, to start in its password manager.
    pub value: Option<ExternalPasswordRef>,
    /// What the entry is for (a host name or address), offered as the first search.
    pub suggestion: Option<String>,
    pub on_pick: EntryPicked,
}

type PickHandler = Rc<dyn Fn(PickRequest, &mut App)>;

/// The window's way of showing the picker for a field.
struct PickHook(PickHandler);

impl Global for PickHook {}

/// Register how a field opens the picker (the app mounts it over the active window).
pub fn set_password_entry_hook(hook: impl Fn(PickRequest, &mut App) + 'static, cx: &mut App) {
    cx.set_global(PickHook(Rc::new(hook)));
}

/// Open the picker for a field; false when no window can show it.
pub fn request_password_entry(request: PickRequest, cx: &mut App) -> bool {
    let Some(hook) = cx.try_global::<PickHook>().map(|hook| hook.0.clone()) else {
        return false;
    };
    hook(request, cx);
    true
}
