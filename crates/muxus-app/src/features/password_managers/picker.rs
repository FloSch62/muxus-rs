//! `PasswordEntryPicker.tsx`: choose one entry from a connected password manager. Only
//! titles, user names and addresses are shown; the password stays in the manager and is
//! read by the backend when a login needs it.

use std::rc::Rc;

use muxus_core::api::{
    ExternalPasswordRef, PasswordManagerEntry, PasswordManagerSource, PasswordManagerState,
};
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::prelude::FluentBuilder;
use muxus_ui::gpui_kit::{
    AnyElement, AnyWindowHandle, App, AppContext, Context, Div, Entity, EventEmitter, FocusHandle,
    Focusable, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, SharedString,
    StatefulInteractiveElement, Styled, Subscription, Window, div, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::{ActiveTheme, Status, Theme};
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::list::ListItemButton;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::typography::{body2, caption, para, text};
use muxus_ui::widgets::{Alert, Button, IconButton};

use super::connect::{
    PasswordManagerDialog, PasswordManagerDialogEvent, open_password_manager_dialog,
};
use super::logic::{
    MAX_ROWS, PICKER_DESCRIPTION, empty_entries_text, entry_caption, entry_reference,
    filter_entries, offered_suggestion, shown_source, state_label, unlocks_in_muxus,
};
use super::store::{self, PasswordManagerList};
use super::unlock::{PasswordManagerUnlockForm, Unlocked};
use crate::features::host_editor::view::form::{self, Choice, Form};
use crate::features::host_editor::widgets::inputs::{InputCache, Menus};
use crate::features::host_editor::widgets::shell::{DIALOG_SM, dialog_frame};

/// What the picker tells its owner.
#[derive(Clone, Debug, PartialEq)]
pub enum PasswordEntryPickerEvent {
    Close,
    Picked(ExternalPasswordRef),
}

/// The entries of the manager being shown.
enum Entries {
    /// Nothing asked yet.
    None,
    Loading,
    Ready(Vec<PasswordManagerEntry>),
    Failed(String),
}

pub struct PasswordEntryPicker {
    backend: Backend,
    /// The entry linked now.
    value: Option<ExternalPasswordRef>,
    suggestion: Option<String>,
    /// The manager chosen in the list; the first one until something is chosen.
    chosen: Option<String>,
    query: Entity<InputState>,
    /// The manager `entries` belongs to.
    entries_of: Option<String>,
    entries: Entries,
    /// The entries are being read again.
    syncing: bool,
    unlock: Option<Entity<PasswordManagerUnlockForm>>,
    connecting: Option<Entity<PasswordManagerDialog>>,
    inputs: InputCache,
    menus: Menus,
    focus: FocusHandle,
    /// The window the picker is in, for what its menus do after they close.
    window: AnyWindowHandle,
    _events: Vec<Subscription>,
    _child_events: Vec<Subscription>,
}

impl EventEmitter<PasswordEntryPickerEvent> for PasswordEntryPicker {}

impl Focusable for PasswordEntryPicker {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl Form for PasswordEntryPicker {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }
}

/// Open the picker, starting in the manager of `value` when there is one.
pub fn open_password_entry_picker(
    backend: Backend,
    value: Option<ExternalPasswordRef>,
    suggestion: Option<String>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<PasswordEntryPicker> {
    cx.new(|cx| PasswordEntryPicker::new(backend, value, suggestion, window, cx))
}

impl PasswordEntryPicker {
    pub fn new(
        backend: Backend,
        value: Option<ExternalPasswordRef>,
        suggestion: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let query = cx.new(|cx| InputState::new(window, cx));
        let events = vec![
            cx.subscribe(
                &query,
                |_: &mut PasswordEntryPicker, _, event: &InputEvent, cx| {
                    if let InputEvent::Change = event {
                        cx.notify();
                    }
                },
            ),
            // The list arrives, or a manager changed state: show what it holds now.
            cx.observe_global_in::<PasswordManagerList>(window, |this, window, cx| {
                this.follow_source(window, cx);
                cx.notify();
            }),
        ];
        // A manager is locked, unlocked and signed in to outside this window too.
        store::refresh(&backend, cx);
        let mut picker = PasswordEntryPicker {
            backend,
            chosen: value.as_ref().map(|value| value.source_id.clone()),
            value,
            suggestion,
            query,
            entries_of: None,
            entries: Entries::None,
            syncing: false,
            unlock: None,
            connecting: None,
            inputs: InputCache::default(),
            menus: Menus::default(),
            focus,
            window: window.window_handle(),
            _events: events,
            _child_events: Vec::new(),
        };
        picker.follow_source(window, cx);
        picker
    }

    /// The titles of the entries listed, as the search narrows them.
    pub fn entry_titles(&self, cx: &App) -> Vec<String> {
        match &self.entries {
            Entries::Ready(entries) => filter_entries(entries, &self.typed(cx))
                .into_iter()
                .take(MAX_ROWS)
                .map(|entry| entry.title)
                .collect(),
            _ => Vec::new(),
        }
    }

    /// Why the entries could not be read, when they could not.
    pub fn entries_error(&self) -> Option<&str> {
        match &self.entries {
            Entries::Failed(error) => Some(error),
            _ => None,
        }
    }

    /// The form that unlocks the manager shown, while it is locked and Muxus can unlock it.
    pub fn unlock_form(&self) -> Option<Entity<PasswordManagerUnlockForm>> {
        self.unlock.clone()
    }

    /// Type `text` into the search.
    pub fn search_for(&mut self, text: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.query
            .update(cx, |state, cx| state.set_value(text.to_owned(), window, cx));
        cx.notify();
    }

    /// The search the picker offers for what the entry is for, when it finds something.
    pub fn suggested_search(&self, cx: &App) -> Option<String> {
        match &self.entries {
            Entries::Ready(entries) => {
                offered_suggestion(entries, self.suggestion.as_deref(), &self.typed(cx))
            }
            _ => None,
        }
    }

    /// Pick the listed entry titled `title`; false when none is.
    pub fn pick_titled(&mut self, title: &str, cx: &mut Context<Self>) -> bool {
        let Entries::Ready(entries) = &self.entries else {
            return false;
        };
        let Some(entry) = filter_entries(entries, &self.typed(cx))
            .into_iter()
            .find(|entry| entry.title == title)
        else {
            return false;
        };
        self.pick(&entry, cx);
        true
    }

    fn source(&self, cx: &App) -> Option<PasswordManagerSource> {
        shown_source(store::listed_sources(cx)?, self.chosen.as_deref()).cloned()
    }

    fn typed(&self, cx: &App) -> String {
        self.query.read(cx).value().to_string()
    }

    /// Bring what is shown in line with the manager now selected: read its entries when it
    /// is ready, offer to unlock it when Muxus can.
    fn follow_source(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let source = self.source(cx);
        let id = source.as_ref().map(|source| source.id.clone());
        if self.entries_of != id {
            self.entries_of = id.clone();
            self.entries = Entries::None;
            self.syncing = false;
        }
        let Some(source) = source else {
            self.unlock = None;
            return;
        };
        let ready = source.state == PasswordManagerState::Ready;
        if ready && matches!(self.entries, Entries::None) {
            self.read_entries(source.id.clone(), false, window, cx);
            // Start typing in the search once there is something to search.
            self.query.update(cx, |state, cx| state.focus(window, cx));
        }
        if !ready {
            // What was listed before it locked is not shown while it is.
            self.entries = Entries::None;
        }
        let unlockable =
            source.state == PasswordManagerState::Locked && unlocks_in_muxus(source.kind);
        if !unlockable {
            self.unlock = None;
        } else if self
            .unlock
            .as_ref()
            .is_none_or(|form| form.read(cx).source_id() != source.id)
        {
            let backend = self.backend.clone();
            let form = cx.new(|cx| PasswordManagerUnlockForm::new(backend, source, window, cx));
            if self.connecting.is_none() {
                let input = form.read(cx).input();
                input.update(cx, |state, cx| state.focus(window, cx));
            }
            self._child_events = vec![cx.subscribe(&form, |_, _, _: &Unlocked, cx| cx.notify())];
            self.unlock = Some(form);
        }
    }

    /// Read the manager's entries; `sync` pulls changes from its server first.
    fn read_entries(
        &mut self,
        id: String,
        sync: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if sync {
            self.syncing = true;
        } else {
            self.entries = Entries::Loading;
        }
        cx.notify();
        let backend = self.backend.clone();
        cx.spawn_in(window, async move |this, cx| {
            let result = backend.password_manager_entries(&id, sync).await;
            let _ = this.update(cx, |this, cx| {
                // Another manager was chosen meanwhile: this answer is not for it.
                if this.entries_of.as_deref() != Some(id.as_str()) {
                    return;
                }
                this.syncing = false;
                match result {
                    Ok(entries) => this.entries = Entries::Ready(entries),
                    Err(error) => {
                        this.entries = Entries::Failed(if error.message.is_empty() {
                            "The entries could not be read.".to_owned()
                        } else {
                            error.message
                        });
                        // Reading can fail because the manager locked itself meanwhile;
                        // the state shown above the list has to say so.
                        store::refresh(&this.backend, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        self.chosen = Some(id);
        self.query
            .update(cx, |state, cx| state.set_value("", window, cx));
        self.follow_source(window, cx);
        cx.notify();
    }

    fn connect(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let installed = store::listed(cx)
            .map(|response| response.installed.clone())
            .unwrap_or_default();
        let dialog =
            open_password_manager_dialog(self.backend.clone(), None, installed, window, cx);
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, event: &PasswordManagerDialogEvent, window, cx| {
                this.connecting = None;
                window.focus(&this.focus, cx);
                if let PasswordManagerDialogEvent::Saved(saved) = event {
                    // The new manager is the one to pick from.
                    this.choose(saved.id.clone(), window, cx);
                }
                cx.notify();
            },
        )
        .detach();
        self.connecting = Some(dialog);
        cx.notify();
    }

    fn pick(&mut self, entry: &PasswordManagerEntry, cx: &mut Context<Self>) {
        if let Some(source) = &self.entries_of {
            cx.emit(PasswordEntryPickerEvent::Picked(entry_reference(
                source, entry,
            )));
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key != "escape" || self.connecting.is_some() {
            return;
        }
        cx.stop_propagation();
        if self.menus.open.is_some() {
            self.menus.close();
            cx.notify();
        } else {
            cx.emit(PasswordEntryPickerEvent::Close);
        }
    }

    /// The search field, with "Read the entries again" at its end.
    fn search(&self, source: &PasswordManagerSource, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let busy = self.syncing || matches!(self.entries, Entries::Loading);
        let end: AnyElement = if busy {
            div()
                .p(px(5.))
                .child(CircularProgress::new(
                    "password-entries-reading",
                    16.,
                    p.primary.main,
                ))
                .into_any_element()
        } else {
            let entity = cx.entity().downgrade();
            let id = source.id.clone();
            IconButton::new("password-entries-refresh", "Refresh")
                .icon_size(muxus_ui::icon::SMALL)
                .tooltip("Read the entries again")
                .on_click(move |_, window, cx| {
                    let _ = entity.update(cx, |this, cx| {
                        this.read_entries(id.clone(), true, window, cx);
                    });
                })
                .into_any_element()
        };
        FormField::input("password-entries-search", &self.query)
            .placeholder(format!("Search {} entries", source.kind.label()))
            .start(icon("Search", muxus_ui::icon::SMALL, p.text.secondary))
            .end(end)
            .into_any_element()
    }

    /// One entry of the list; every one but the last is ruled off from the next.
    fn row(
        &self,
        index: usize,
        last: bool,
        entry: &PasswordManagerEntry,
        source_id: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let selected = self
            .value
            .as_ref()
            .is_some_and(|value| value.source_id == source_id && value.entry_id == entry.id);
        let mut texts = div().flex().flex_col().flex_1().min_w_0().child(text(
            entry.title.clone(),
            body2(),
            p.text.primary,
        ));
        if let Some(detail) = entry_caption(entry) {
            texts = texts.child(text(detail, caption(), p.text.secondary));
        }
        let entity = cx.entity().downgrade();
        let picked = entry.clone();
        let mut row = ListItemButton::new(("password-entry", index))
            .selected(selected)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::spacing(1.25))
                    .w_full()
                    .min_w_0()
                    .child(
                        div()
                            .flex_none()
                            .child(icon("KeyOutlined", 16., p.text.secondary)),
                    )
                    .child(texts)
                    .when_some(
                        entry.group.clone().filter(|group| !group.is_empty()),
                        |row, group| {
                            row.child(
                                div()
                                    .flex_none()
                                    .max_w(muxus_ui::gpui_kit::relative(0.4))
                                    .child(text(group, caption(), p.text.secondary)),
                            )
                        },
                    ),
            )
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| this.pick(&picked, cx));
            });
        if !last {
            row = row.divider();
        }
        row.into_any_element()
    }
}

/// What stands where the list would be while there is none.
fn centered(theme: &Theme) -> Div {
    let _ = theme;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .items_center()
        .justify_center()
        .py(Theme::spacing(4.))
}

impl Render for PasswordEntryPicker {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let sources: Option<Vec<PasswordManagerSource>> =
            store::listed_sources(cx).map(<[_]>::to_vec);
        let mut content = div().flex().flex_col().min_h(px(420.)).child(
            div()
                .mb(Theme::spacing(2.))
                .child(para(PICKER_DESCRIPTION, body2(), p.text.secondary)),
        );

        match &sources {
            None => {
                content = content.child(centered(&theme).child(CircularProgress::new(
                    "password-managers-loading",
                    22.,
                    p.primary.main,
                )));
            }
            Some(sources) if sources.is_empty() => {
                let entity = cx.entity().downgrade();
                content = content.child(
                    centered(&theme)
                        .child(div().mb(Theme::spacing(1.5)).child(text(
                            "No password manager is connected yet.",
                            body2(),
                            p.text.secondary,
                        )))
                        .child(
                            Button::new("password-entry-connect", "Connect a password manager")
                                .contained()
                                .start_icon("Add")
                                .on_click(move |_, window, cx| {
                                    let _ = entity.update(cx, |this, cx| this.connect(window, cx));
                                }),
                        ),
                );
            }
            Some(sources) => {
                let source = shown_source(sources, self.chosen.as_deref()).cloned();
                let current = source
                    .as_ref()
                    .map(|source| source.id.clone())
                    .unwrap_or_default();
                let choices = sources
                    .iter()
                    .map(|candidate| {
                        let state = state_label(candidate.state);
                        Choice::new(
                            candidate.id.clone(),
                            if state.is_empty() {
                                candidate.name.clone()
                            } else {
                                format!("{}   {state}", candidate.name)
                            },
                        )
                    })
                    .collect();
                let shown_name = source
                    .as_ref()
                    .map(|source| source.name.clone())
                    .unwrap_or_default();
                let select = form::select(
                    self,
                    "password-entry-source",
                    |body| {
                        FormField::new("password-entry-source-field", body)
                            .label("Password manager")
                            .shrink(true)
                    },
                    &current,
                    choices,
                    Some(FieldBody::Value(shown_name.into())),
                    window,
                    cx,
                    |this, id, cx| {
                        // Once the menu has closed: its handler has no window to work in.
                        let (window, picker) = (this.window, cx.entity().downgrade());
                        cx.defer(move |cx| {
                            let _ = window.update(cx, |_, window, cx| {
                                let _ = picker.update(cx, |this, cx| this.choose(id, window, cx));
                            });
                        });
                    },
                );
                let connect = cx.entity().downgrade();
                content = content.child(
                    div()
                        .flex()
                        .items_start()
                        .gap(Theme::spacing(1.))
                        .mb(Theme::spacing(1.5))
                        .child(div().flex_1().min_w_0().child(select))
                        .child(
                            div().mt(Theme::spacing(0.75)).child(
                                IconButton::new("password-entry-connect-another", "Add")
                                    .medium()
                                    .icon_size(muxus_ui::icon::SMALL)
                                    .tooltip("Connect another password manager")
                                    .on_click(move |_, window, cx| {
                                        let _ =
                                            connect.update(cx, |this, cx| this.connect(window, cx));
                                    }),
                            ),
                        ),
                );

                if let Some(source) = &source {
                    if source.state != PasswordManagerState::Ready {
                        content = content.child(match &self.unlock {
                            Some(form) => form.clone().into_any_element(),
                            None => {
                                let backend = self.backend.clone();
                                Alert::new(
                                    if source.state == PasswordManagerState::Unavailable {
                                        Status::Error
                                    } else {
                                        Status::Warning
                                    },
                                    source.message.clone().unwrap_or_else(|| {
                                        format!("{} is not available.", source.name)
                                    }),
                                )
                                .outlined()
                                .action(
                                    Button::new("password-entry-check-again", "Check again")
                                        .on_click(move |_, _, cx| store::refresh(&backend, cx)),
                                )
                                .into_any_element()
                            }
                        });
                    } else {
                        content = content.child(self.search(source, cx));
                        let query = self.typed(cx);
                        let listed: &[PasswordManagerEntry] = match &self.entries {
                            Entries::Ready(entries) => entries,
                            _ => &[],
                        };
                        if let Some(suggested) =
                            offered_suggestion(listed, self.suggestion.as_deref(), &query)
                        {
                            let entity = cx.entity().downgrade();
                            let search = suggested.clone();
                            content = content.child(
                                div().mt(Theme::spacing(0.75)).flex().child(
                                    Button::new(
                                        "password-entry-suggestion",
                                        format!("Show entries matching “{suggested}”"),
                                    )
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = entity.update(cx, |this, cx| {
                                                this.query.update(cx, |state, cx| {
                                                    state.set_value(search.clone(), window, cx)
                                                });
                                                cx.notify();
                                            });
                                        },
                                    ),
                                ),
                            );
                        }
                        match &self.entries {
                            Entries::Failed(error) => {
                                content =
                                    content.child(div().mt(Theme::spacing(1.5)).child(
                                        Alert::new(Status::Error, error.clone()).outlined(),
                                    ));
                            }
                            Entries::None | Entries::Loading => {
                                content = content.child(
                                    centered(&theme)
                                        .child(CircularProgress::new(
                                            "password-entries-loading",
                                            22.,
                                            p.primary.main,
                                        ))
                                        .child(div().mt(Theme::spacing(1.5)).child(text(
                                            format!("Reading entries from {}…", source.name),
                                            caption(),
                                            p.text.secondary,
                                        ))),
                                );
                            }
                            Entries::Ready(entries) => {
                                let matches = filter_entries(entries, &query);
                                if matches.is_empty() {
                                    content = content.child(centered(&theme).child(text(
                                        empty_entries_text(&query),
                                        body2(),
                                        p.text.secondary,
                                    )));
                                } else {
                                    let shown = matches.len().min(MAX_ROWS);
                                    let rows: Vec<AnyElement> = matches
                                        .iter()
                                        .take(MAX_ROWS)
                                        .enumerate()
                                        .map(|(index, entry)| {
                                            self.row(
                                                index,
                                                index + 1 == shown,
                                                entry,
                                                &source.id,
                                                cx,
                                            )
                                        })
                                        .collect();
                                    content = content.child(
                                        div()
                                            .id("password-entries")
                                            .mt(Theme::spacing(1.))
                                            .flex()
                                            .flex_col()
                                            .flex_1()
                                            .min_h_0()
                                            .max_h(px(360.))
                                            .overflow_y_scroll()
                                            .border_1()
                                            .border_color(p.divider)
                                            .rounded(px(theme.radius * 1.5))
                                            .children(rows),
                                    );
                                    if matches.len() > MAX_ROWS {
                                        content = content.child(
                                            div().mt(Theme::spacing(0.75)).child(text(
                                                format!(
                                                    "Showing {MAX_ROWS} of {} entries. Search to narrow them down.",
                                                    matches.len()
                                                ),
                                                caption(),
                                                p.text.secondary,
                                            )),
                                        );
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        let (cancel, close) = (cx.entity().downgrade(), cx.entity().downgrade());
        let actions = vec![
            Button::new("password-entry-cancel", "Cancel")
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |_, cx| cx.emit(PasswordEntryPickerEvent::Close));
                })
                .into_any_element(),
        ];
        let mut root = div()
            .id("password-entry-picker")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "password-entry-picker-backdrop",
                dialog_frame(
                    &theme,
                    window,
                    "password-entry-picker-content",
                    DIALOG_SM,
                    SharedString::from("Choose a password-manager entry"),
                    6.,
                    content.into_any_element(),
                    actions,
                    true,
                ),
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |_, cx| cx.emit(PasswordEntryPickerEvent::Close));
                })),
            ));
        if let Some(connecting) = &self.connecting {
            root = root.child(connecting.clone());
        }
        root
    }
}
