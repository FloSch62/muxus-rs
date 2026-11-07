//! `components/ScriptsDialog.tsx`: run a script on many SSH hosts, all at once, in batches or
//! one by one. Scripts are kept in Muxus; a local file can be loaded into one. Runs go on in
//! the window's [`ScriptJobs`], so closing the dialog does not stop them.

use std::rc::Rc;

use muxus_core::api::{MAX_SCRIPT_BYTES, SavedScript, SavedScriptInput};
use muxus_core::js::js_trim;
use muxus_session::Backend;
use muxus_session::scripts::{HostRunState, ScriptTarget};
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState, Textarea, TextareaState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, ClipboardItem, Context, Entity, EventEmitter, FocusHandle,
    Focusable, Hsla, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    PathPromptOptions, Render, ScrollHandle, SharedString, StatefulInteractiveElement, Styled,
    Subscription, Window, div, px,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts};
use muxus_ui::widgets::autocomplete::{Autocomplete, AutocompleteOption};
use muxus_ui::widgets::dialog::{dialog_paper, dialog_title, modal};
use muxus_ui::widgets::display::divider;
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel};
use muxus_ui::widgets::form_field::{FieldBody, FormField};
use muxus_ui::widgets::list::{ListItemButton, list_item_icon, list_item_text};
use muxus_ui::widgets::tabs::{Tab, tabs_row};
use muxus_ui::widgets::{Button, ButtonColor, Chip, IconButton, TextField};

use super::jobs::ScriptJobs;
use super::model::{
    JobInput, ModeKind, NEW_SCRIPT_BODY, host_heading, host_summary, line_count_label, mode_label,
    run_label, run_mode, run_problem, script_name_from_file, script_targets, timeout_seconds,
    with_keepalive,
};
use crate::features::launcher::kit::{
    AskAnswer, AskDialog, AskRequest, show_error_toast, show_toast, text,
};
use crate::features::settings::view::fields::style_textarea;
use crate::hosts::ManagedHost;
use crate::util::file_dialog::pick_paths;

/// What the dialog asks the window to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ScriptsDialogEvent {
    Close,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum View {
    Script,
    Results,
}

/// What a confirmation is about.
#[derive(Clone, Debug)]
enum Pending {
    /// Switch to this script (`None`: a new one).
    Pick(Option<String>),
    Close,
    Delete(String),
}

pub struct ScriptsDialog {
    backend: Backend,
    jobs: Entity<ScriptJobs>,
    /// Hosts scripts can run on, by name.
    targets: Vec<ScriptTarget>,
    keepalive: Option<f64>,
    view: View,
    scripts: Vec<SavedScript>,
    /// The script in the editor; `None` is a new one.
    selected: Option<String>,
    name: Entity<InputState>,
    body: Entity<TextareaState>,
    dirty: bool,
    saving: bool,
    chosen: Vec<String>,
    host_search: Entity<InputState>,
    mode: ModeKind,
    batch: Entity<InputState>,
    stop_on_failure: bool,
    timeout: Entity<InputState>,
    /// The host whose output the results show.
    shown_host: Option<String>,
    output_scroll: ScrollHandle,
    /// The output shown last frame (job, host and length), to follow new output.
    shown_output: (u64, String, usize),
    output_text: SharedString,
    ask: Option<(Entity<AskDialog>, Pending)>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<ScriptsDialogEvent> for ScriptsDialog {}

impl Focusable for ScriptsDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

/// Open Scripts. `hosts` are every host of the window, `host_keys` the ones to pick (a host,
/// a selection or a folder); `keepalive` is the SSH keepalive preference the dials carry.
pub fn open_scripts_dialog(
    backend: Backend,
    jobs: Entity<ScriptJobs>,
    hosts: &[ManagedHost],
    host_keys: Option<Vec<String>>,
    keepalive: Option<f64>,
    window: &mut Window,
    cx: &mut App,
) -> Entity<ScriptsDialog> {
    let targets = script_targets(hosts);
    cx.new(|cx| ScriptsDialog::new(backend, jobs, targets, host_keys, keepalive, window, cx))
}

/// Keep only what `keep` allows in an input, as the client's `replace(/[^\d]/g, '')` does.
fn filter_input(
    input: &Entity<InputState>,
    keep: fn(char) -> bool,
    window: &mut Window,
    cx: &mut App,
) {
    let value = input.read(cx).value().to_string();
    let filtered: String = value.chars().filter(|c| keep(*c)).collect();
    if filtered != value {
        input.update(cx, |state, cx| state.set_value(filtered, window, cx));
    }
}

impl ScriptsDialog {
    pub fn new(
        backend: Backend,
        jobs: Entity<ScriptJobs>,
        targets: Vec<ScriptTarget>,
        host_keys: Option<Vec<String>>,
        keepalive: Option<f64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let input = |value: &str, window: &mut Window, cx: &mut Context<Self>| {
            let value = value.to_owned();
            cx.new(|cx| {
                let mut state = InputState::new(window, cx);
                state.set_value(value, window, cx);
                state
            })
        };
        let name = input("", window, cx);
        let host_search = input("", window, cx);
        let batch = input("5", window, cx);
        let timeout = input("30", window, cx);
        let body = cx.new(|cx| {
            let mut state = TextareaState::new(window, cx)
                .auto_grow(10, 16)
                .soft_wrap(false);
            state.set_value(NEW_SCRIPT_BODY, window, cx);
            state
        });
        let subscriptions = vec![
            cx.observe(&jobs, |_, _, cx| cx.notify()),
            cx.subscribe(&name, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.dirty = true;
                    cx.notify();
                }
            }),
            cx.subscribe(&body, |this, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.dirty = true;
                    cx.notify();
                }
            }),
            cx.subscribe_in(
                &batch,
                window,
                |this, batch, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        filter_input(batch, |c| c.is_ascii_digit(), window, cx);
                        this.mode = ModeKind::Batches;
                        cx.notify();
                    }
                },
            ),
            cx.subscribe_in(
                &timeout,
                window,
                |_, timeout, event: &InputEvent, window, cx| {
                    if matches!(event, InputEvent::Change) {
                        filter_input(timeout, |c| c.is_ascii_digit() || c == '.', window, cx);
                        cx.notify();
                    }
                },
            ),
            cx.subscribe(&host_search, |_, _, _: &InputEvent, cx| cx.notify()),
        ];
        let view = if jobs.read(cx).running() {
            View::Results
        } else {
            View::Script
        };
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut dialog = Self {
            backend,
            jobs,
            targets,
            keepalive,
            view,
            scripts: Vec::new(),
            selected: None,
            name,
            body,
            dirty: false,
            saving: false,
            chosen: host_keys.unwrap_or_default(),
            host_search,
            mode: ModeKind::Parallel,
            batch,
            stop_on_failure: false,
            timeout,
            shown_host: None,
            output_scroll: ScrollHandle::new(),
            shown_output: (0, String::new(), 0),
            output_text: SharedString::default(),
            ask: None,
            focus,
            _subscriptions: subscriptions,
        };
        dialog.load_scripts(cx);
        dialog
    }

    /// Type a script into the editor, as the user would.
    pub fn edit_script(
        &mut self,
        name: &str,
        body: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let (name, body) = (name.to_owned(), body.to_owned());
        self.name
            .update(cx, |state, cx| state.set_value(name, window, cx));
        self.body
            .update(cx, |state, cx| state.set_value(body, window, cx));
        self.dirty = true;
        cx.notify();
    }

    /// The scripts kept in Muxus, as listed.
    pub fn saved_scripts(&self) -> &[SavedScript] {
        &self.scripts
    }

    /// The hosts picked to run on.
    pub fn chosen(&self) -> &[String] {
        &self.chosen
    }

    /// Results are on screen (after Run, or while a job runs).
    pub fn showing_results(&self) -> bool {
        self.view == View::Results
    }

    /// A confirmation (discard, delete) is open.
    pub fn asking(&self) -> bool {
        self.ask.is_some()
    }

    fn load_scripts(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.scripts().await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(response) => {
                    this.scripts = response.scripts;
                    cx.notify();
                }
                Err(error) => show_error_toast(error.message, cx),
            });
        })
        .detach();
    }

    fn name_value(&self, cx: &App) -> String {
        self.name.read(cx).value().to_string()
    }

    fn body_value(&self, cx: &App) -> String {
        self.body.read(cx).value().to_string()
    }

    /// Put a script (or a new one) in the editor.
    fn pick(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        let script = id
            .as_ref()
            .and_then(|id| self.scripts.iter().find(|script| &script.id == id))
            .cloned();
        self.selected = script.as_ref().map(|script| script.id.clone());
        let (name, body) = match script {
            Some(script) => (script.name, script.body),
            None => (String::new(), NEW_SCRIPT_BODY.to_owned()),
        };
        self.name
            .update(cx, |state, cx| state.set_value(name, window, cx));
        self.body
            .update(cx, |state, cx| state.set_value(body, window, cx));
        self.dirty = false;
        cx.notify();
    }

    fn ask(
        &mut self,
        request: AskRequest,
        pending: Pending,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let dialog = cx.new(|cx| AskDialog::new(request, window, cx));
        cx.subscribe_in(
            &dialog,
            window,
            |this, _, answer: &AskAnswer, window, cx| {
                let Some((_, pending)) = this.ask.take() else {
                    return;
                };
                window.focus(&this.focus, cx);
                if *answer == AskAnswer::Confirmed {
                    match pending {
                        Pending::Pick(id) => this.pick(id, window, cx),
                        Pending::Close => cx.emit(ScriptsDialogEvent::Close),
                        Pending::Delete(id) => this.delete(id, window, cx),
                    }
                }
                cx.notify();
            },
        )
        .detach();
        self.ask = Some((dialog, pending));
        cx.notify();
    }

    fn discard_request() -> AskRequest {
        AskRequest {
            title: "Discard changes?".into(),
            description: Some("The script has unsaved changes.".into()),
            confirm_label: "Discard".into(),
            destructive: true,
            prompt: None,
        }
    }

    /// Switch scripts, asking first when the editor has unsaved changes.
    fn choose(&mut self, id: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.ask(Self::discard_request(), Pending::Pick(id), window, cx);
        } else {
            self.pick(id, window, cx);
        }
    }

    /// Closing throws away an unsaved script, so ask first, as switching does.
    pub fn close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dirty {
            self.ask(Self::discard_request(), Pending::Close, window, cx);
        } else {
            cx.emit(ScriptsDialogEvent::Close);
        }
    }

    /// "Save": keep the script in Muxus.
    pub fn save(&mut self, cx: &mut Context<Self>) {
        let (name, body) = (self.name_value(cx), self.body_value(cx));
        if self.saving || js_trim(&name).is_empty() || js_trim(&body).is_empty() {
            return;
        }
        self.saving = true;
        let input = SavedScriptInput {
            id: self.selected.clone(),
            name,
            body,
        };
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            let result = backend.save_script(input).await;
            let _ = this.update(cx, |this, cx| {
                this.saving = false;
                match result {
                    Ok(script) => {
                        this.selected = Some(script.id.clone());
                        this.dirty = false;
                        show_toast(Status::Success, format!("Saved “{}”.", script.name), cx);
                        this.load_scripts(cx);
                    }
                    Err(error) => show_error_toast(error.message, cx),
                }
                cx.notify();
            });
        })
        .detach();
        cx.notify();
    }

    fn request_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.selected.clone() else {
            return;
        };
        let name = self.name_value(cx);
        self.ask(
            AskRequest {
                title: format!("Delete “{name}”?"),
                description: Some("The script is removed from Muxus.".into()),
                confirm_label: "Delete".into(),
                destructive: true,
                prompt: None,
            },
            Pending::Delete(id),
            window,
            cx,
        );
    }

    fn delete(&mut self, id: String, window: &mut Window, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let result = backend.delete_script(&id).await;
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| match result {
                    Ok(_) => {
                        this.pick(None, window, cx);
                        this.load_scripts(cx);
                    }
                    Err(error) => show_error_toast(error.message, cx),
                });
            });
        })
        .detach();
    }

    /// "From file…": load a script file from this computer into the editor.
    fn import_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        let window_handle = window.window_handle();
        cx.spawn(async move |this, cx| {
            let Some(path) = picked.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            let read = std::fs::metadata(&path).and_then(|meta| {
                if meta.len() > MAX_SCRIPT_BYTES as u64 {
                    Ok(None)
                } else {
                    std::fs::read(&path).map(Some)
                }
            });
            let _ = window_handle.update(cx, |_, window, cx| {
                let _ = this.update(cx, |this, cx| match read {
                    Ok(None) => show_error_toast("Scripts can be up to 1 MB.", cx),
                    Ok(Some(bytes)) => {
                        let file_name = path
                            .file_name()
                            .map(|name| name.to_string_lossy().into_owned())
                            .unwrap_or_default();
                        let body = String::from_utf8_lossy(&bytes).into_owned();
                        this.selected = None;
                        this.name.update(cx, |state, cx| {
                            state.set_value(script_name_from_file(&file_name), window, cx)
                        });
                        this.body
                            .update(cx, |state, cx| state.set_value(body, window, cx));
                        this.dirty = true;
                        cx.notify();
                    }
                    Err(error) => show_error_toast(error.to_string(), cx),
                });
            });
        })
        .detach();
    }

    /// The picked hosts that still exist, in the order they were picked.
    fn chosen_targets(&self) -> Vec<ScriptTarget> {
        self.chosen
            .iter()
            .filter_map(|key| self.targets.iter().find(|target| &target.key == key))
            .cloned()
            .collect()
    }

    fn problem(&self, cx: &App) -> Option<String> {
        run_problem(
            &self.body_value(cx),
            self.chosen_targets().len(),
            self.mode,
            &self.batch.read(cx).value(),
            &self.timeout.read(cx).value(),
            self.jobs.read(cx).running(),
        )
    }

    /// "Run on n hosts".
    pub fn run(&mut self, cx: &mut Context<Self>) {
        if self.problem(cx).is_some() {
            return;
        }
        let name = self.name_value(cx);
        let input = JobInput {
            script_name: if js_trim(&name).is_empty() {
                "Untitled script".into()
            } else {
                js_trim(&name).to_owned()
            },
            body: self.body_value(cx),
            targets: self
                .chosen_targets()
                .into_iter()
                .map(|target| with_keepalive(target, self.keepalive))
                .collect(),
            mode: run_mode(self.mode, &self.batch.read(cx).value()),
            stop_on_failure: self.mode != ModeKind::Parallel && self.stop_on_failure,
            timeout_seconds: timeout_seconds(&self.timeout.read(cx).value()).unwrap_or(0),
        };
        match self.jobs.update(cx, |jobs, cx| jobs.start(input, cx)) {
            Ok(()) => {
                self.view = View::Results;
                self.shown_host = None;
            }
            Err(message) => show_error_toast(message, cx),
        }
        cx.notify();
    }

    fn rerun(&mut self, cx: &mut Context<Self>) {
        let targets: Vec<ScriptTarget> = self
            .targets
            .iter()
            .cloned()
            .map(|target| with_keepalive(target, self.keepalive))
            .collect();
        if let Err(message) = self
            .jobs
            .update(cx, |jobs, cx| jobs.rerun_failed(&targets, cx))
        {
            show_error_toast(message, cx);
        }
        self.shown_host = None;
        cx.notify();
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" && self.ask.is_none() {
            cx.stop_propagation();
            self.close(window, cx);
        }
    }

    // --- the script and its hosts ---------------------------------------------------------

    fn script_list(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let t = theme.typography;
        let mut list = div()
            .id("scripts-list")
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .py(px(8.))
            .overflow_y_scroll();
        if self.scripts.is_empty() {
            list = list.child(
                div()
                    .px(Theme::spacing(2.))
                    .py(Theme::spacing(1.))
                    .child(text("No saved scripts yet.", t.body2, p.text.secondary)),
            );
        }
        for (index, script) in self.scripts.iter().enumerate() {
            let id = script.id.clone();
            list = list.child(
                ListItemButton::new(("scripts-saved", index))
                    .selected(self.selected.as_deref() == Some(script.id.as_str()))
                    .child(list_item_text(
                        text(script.name.clone(), t.body2, p.text.primary),
                        Some(
                            text(line_count_label(&script.body), t.caption, p.text.secondary)
                                .into_any_element(),
                        ),
                    ))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose(Some(id.clone()), window, cx)
                    })),
            );
        }
        div()
            .flex()
            .flex_col()
            .w(px(230.))
            .flex_none()
            .min_h_0()
            .border_r_1()
            .border_color(p.divider)
            .child(
                div()
                    .flex()
                    .gap(Theme::spacing(1.))
                    .p(Theme::spacing(1.))
                    .child(
                        Button::new("scripts-new", "New")
                            .start_icon("Add")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.choose(None, window, cx)),
                            ),
                    )
                    .child(
                        Button::new("scripts-from-file", "From file…")
                            .start_icon("UploadFileOutlined")
                            .on_click(
                                cx.listener(|this, _, window, cx| this.import_file(window, cx)),
                            ),
                    ),
            )
            .child(divider(theme))
            .child(list)
            .into_any_element()
    }

    fn host_picker(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let chosen = self.chosen_targets();
        let options: Vec<AutocompleteOption> = self
            .targets
            .iter()
            .filter(|target| !self.chosen.contains(&target.key))
            .map(|target| AutocompleteOption::new(target.label.clone()).value(target.key.clone()))
            .collect();
        let mut chips = div().flex().flex_wrap().gap(px(4.));
        for (index, target) in chosen.iter().enumerate() {
            let key = target.key.clone();
            chips = chips.child(Chip::new(target.label.clone()).on_delete(
                ("scripts-host-chip", index),
                "Cancel",
                None,
                cx.listener(move |this, _, _, cx| {
                    this.chosen.retain(|chosen| chosen != &key);
                    cx.notify();
                }),
            ));
        }
        let entity = cx.entity().downgrade();
        let search = self.host_search.clone();
        let picker = Autocomplete::new(
            "scripts-hosts",
            &self.host_search,
            FormField::input("scripts-hosts-field", &self.host_search)
                .label(format!(
                    "Run on ({} of {} SSH hosts)",
                    chosen.len(),
                    self.targets.len()
                ))
                .placeholder("Pick hosts"),
            options,
        )
        .disable_clearable()
        .on_pick(move |option, window, cx| {
            let key = option.value.clone();
            search.update(cx, |state, cx| state.set_value("", window, cx));
            let _ = entity.update(cx, |this, cx| {
                if !this.chosen.contains(&key) {
                    this.chosen.push(key);
                }
                cx.notify();
            });
        });
        let mut column = div().flex().flex_col().gap(Theme::spacing(1.));
        if !chosen.is_empty() {
            column = column.child(chips);
        }
        column
            .child(picker)
            .child(
                div()
                    .flex()
                    .gap(Theme::spacing(1.))
                    .child(
                        Button::new("scripts-all-hosts", "All SSH hosts").on_click(cx.listener(
                            |this, _, _, cx| {
                                this.chosen = this.targets.iter().map(|t| t.key.clone()).collect();
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        Button::new("scripts-clear-hosts", "Clear")
                            .disabled(self.chosen.is_empty())
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.chosen.clear();
                                cx.notify();
                            })),
                    ),
            )
            .text_color(p.text.primary)
            .into_any_element()
    }

    fn run_options(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let t = theme.typography;
        let radio =
            |id: &'static str, kind: ModeKind, label: &'static str, cx: &mut Context<Self>| {
                FormControlLabel::new(id, FormControl::Radio(self.mode == kind), label)
                    .small()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.mode = kind;
                        cx.notify();
                    }))
            };
        let modes = div()
            .flex()
            .flex_col()
            .child(radio(
                "scripts-mode-parallel",
                ModeKind::Parallel,
                "All at once",
                cx,
            ))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(Theme::spacing(1.))
                    .child(radio(
                        "scripts-mode-batches",
                        ModeKind::Batches,
                        "In batches of",
                        cx,
                    ))
                    .child(FormField::input("scripts-batch-size", &self.batch).width(72.))
                    .child(text("hosts", t.body2, p.text.primary)),
            )
            .child(radio(
                "scripts-mode-sequential",
                ModeKind::Sequential,
                "One by one",
                cx,
            ));
        let parallel = self.mode == ModeKind::Parallel;
        let timeout_problem = timeout_seconds(&self.timeout.read(cx).value()).is_err();
        let limits = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(1.))
            .child(
                FormControlLabel::new(
                    "scripts-stop-on-failure",
                    FormControl::Checkbox(self.stop_on_failure && !parallel),
                    "Stop at the first host that fails",
                )
                .disabled(parallel)
                .on_click(cx.listener(|this, _, _, cx| {
                    this.stop_on_failure = !this.stop_on_failure;
                    cx.notify();
                })),
            )
            .child(
                FormField::input("scripts-timeout", &self.timeout)
                    .label("Stop a host's run after (minutes)")
                    .helper("0 lets it run")
                    .error(timeout_problem)
                    .width(260.),
            );
        div()
            .flex()
            .flex_wrap()
            .items_start()
            .gap(Theme::spacing(3.))
            .child(modes)
            .child(limits)
            .into_any_element()
    }

    fn script_view(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let t = theme.typography;
        let (name, body) = (self.name_value(cx), self.body_value(cx));
        let save_label = if self.selected.is_none() {
            "Save"
        } else if self.dirty {
            "Save changes"
        } else {
            "Saved"
        };
        let mut name_row = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(1.))
            .child(
                div().flex_1().min_w_0().child(
                    TextField::new("scripts-name", &self.name)
                        .label("Name")
                        .full_width(),
                ),
            )
            .child(
                Button::new("scripts-save", save_label)
                    .outlined()
                    .disabled(js_trim(&name).is_empty() || js_trim(&body).is_empty() || self.saving)
                    .on_click(cx.listener(|this, _, _, cx| this.save(cx))),
            );
        if self.selected.is_some() {
            name_row = name_row.child(
                IconButton::new("scripts-delete", "DeleteOutlined")
                    .tooltip("Delete script")
                    .on_click(cx.listener(|this, _, window, cx| this.request_delete(window, cx))),
            );
        }

        style_textarea(&self.body, theme, cx);
        let focus = self.body.read(cx).focus_handle(cx);
        let editor = FormField::new(
            "scripts-body",
            FieldBody::Element(
                div()
                    .font_family(fonts::MONO)
                    .text_size(px(13.))
                    .child(Textarea::new(&self.body))
                    .into_any_element(),
            ),
        )
        .multiline()
        .min_rows(10)
        .element_focus(focus)
        .label("Script")
        .shrink(true)
        .notch(p.background_paper)
        .helper(
            "Runs with sh, or with the interpreter its #! line names (bash, python3 …). It gets an empty stdin.",
        );

        let problem = self.problem(cx);
        let count = self.chosen_targets().len();
        let mut run_row = div().flex().items_center().gap(Theme::spacing(2.)).child(
            Button::new("scripts-run", run_label(count))
                .contained()
                .start_icon("PlayArrow")
                .disabled(problem.is_some())
                .on_click(cx.listener(|this, _, _, cx| this.run(cx))),
        );
        if let Some(problem) = problem {
            run_row = run_row.child(text(problem, t.body2, p.text.secondary));
        }

        div()
            .flex()
            .flex_1()
            .min_h_0()
            .child(self.script_list(theme, cx))
            .child(
                div()
                    .id("scripts-form")
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .min_h_0()
                    .p(Theme::spacing(2.))
                    .gap(Theme::spacing(2.))
                    .overflow_y_scroll()
                    .child(name_row)
                    .child(editor)
                    .child(self.host_picker(theme, cx))
                    .child(self.run_options(theme, cx))
                    .child(run_row),
            )
            .into_any_element()
    }

    // --- results ---------------------------------------------------------------------------

    fn results_view(&mut self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let t = theme.typography;
        // What the view needs of the job, without copying every host's output each frame.
        let (overview, rows, shown) = {
            let jobs = self.jobs.read(cx);
            let Some(job) = jobs.job.as_ref() else {
                return div().into_any_element();
            };
            let count = |states: &[HostRunState]| job.count(states);
            let overview = Overview {
                running: job.running(),
                succeeded: count(&[HostRunState::Completed]),
                failed: count(&[HostRunState::Failed]),
                to_go: count(&[
                    HostRunState::Queued,
                    HostRunState::Connecting,
                    HostRunState::Running,
                ]),
                not_run: count(&[HostRunState::Skipped, HostRunState::Cancelled]),
                mode: format!(
                    "{}{}",
                    mode_label(job.mode),
                    if job.stop_on_failure {
                        ", stopping at the first failure"
                    } else {
                        ""
                    }
                ),
            };
            let rows: Vec<(String, String, HostRunState, String)> = job
                .hosts
                .iter()
                .map(|run| {
                    (
                        run.key.clone(),
                        run.label.clone(),
                        run.state,
                        host_summary(run),
                    )
                })
                .collect();
            let shown = job
                .hosts
                .iter()
                .find(|host| Some(&host.key) == self.shown_host.as_ref())
                .or(job.hosts.first())
                .map(|host| {
                    let changed =
                        self.shown_output != (job.id, host.key.clone(), host.output.len());
                    ShownHost {
                        job: job.id,
                        key: host.key.clone(),
                        heading: host_heading(host),
                        state: host.state,
                        error: host.error.clone(),
                        output_len: host.output.len(),
                        output: changed.then(|| host.output.clone()),
                    }
                });
            (overview, rows, shown)
        };

        let mut summary = div()
            .flex()
            .flex_wrap()
            .items_center()
            .gap(Theme::spacing(1.))
            .px(Theme::spacing(2.))
            .py(Theme::spacing(1.))
            .child(
                Chip::new(format!("{} succeeded", overview.succeeded))
                    .outlined()
                    .palette(p.success),
            )
            .child(
                Chip::new(format!("{} failed", overview.failed))
                    .outlined()
                    .palette(p.error),
            )
            .child(Chip::new(format!("{} to go", overview.to_go)).outlined());
        if overview.not_run > 0 {
            summary = summary.child(Chip::new(format!("{} not run", overview.not_run)).outlined());
        }
        summary = summary.child(div().flex_1().min_w_0().child(text(
            overview.mode,
            t.body2,
            p.text.secondary,
        )));
        if overview.running {
            summary = summary.child(
                Button::new("scripts-cancel", "Cancel")
                    .color(ButtonColor::Error)
                    .start_icon("Stop")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jobs.update(cx, |jobs, cx| jobs.cancel(cx))
                    })),
            );
        } else if overview.failed > 0 {
            summary = summary.child(
                Button::new("scripts-rerun", "Run again on failed hosts")
                    .start_icon("Redo")
                    .on_click(cx.listener(|this, _, _, cx| this.rerun(cx))),
            );
        }

        let mut list = div()
            .id("scripts-results-hosts")
            .flex()
            .flex_col()
            .w(px(280.))
            .flex_none()
            .min_h_0()
            .py(px(8.))
            .border_r_1()
            .border_color(p.divider)
            .overflow_y_scroll();
        for (index, (key, label, state, line)) in rows.into_iter().enumerate() {
            let selected = shown.as_ref().is_some_and(|host| host.key == key);
            list = list.child(
                ListItemButton::new(("scripts-result-host", index))
                    .selected(selected)
                    .child(list_item_icon(32., state_icon(state, theme)))
                    .child(list_item_text(
                        text(label, t.body2, p.text.primary),
                        Some(text(line, t.caption, p.text.secondary).into_any_element()),
                    ))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.shown_host = Some(key.clone());
                        cx.notify();
                    })),
            );
        }

        div()
            .flex()
            .flex_col()
            .flex_1()
            .min_h_0()
            .child(summary)
            .child(divider(theme))
            .child(
                div()
                    .flex()
                    .flex_1()
                    .min_h_0()
                    .child(list)
                    .child(self.output_pane(shown, theme)),
            )
            .into_any_element()
    }

    fn output_pane(&mut self, host: Option<ShownHost>, theme: &Theme) -> AnyElement {
        let p = theme.palette;
        let t = theme.typography;
        let Some(host) = host else {
            return div().flex_1().into_any_element();
        };
        // New output (or another host): follow it while the view is at the bottom.
        if let Some(output) = host.output {
            let near_bottom = {
                let offset = self.output_scroll.offset().y;
                let max = self.output_scroll.max_offset().y;
                max + offset < px(80.)
            };
            if near_bottom || self.shown_output.1 != host.key {
                self.output_scroll.scroll_to_bottom();
            }
            self.shown_output = (host.job, host.key.clone(), host.output_len);
            self.output_text = output.into();
        }
        let empty = host.output_len == 0;
        let shown: SharedString = if !empty {
            self.output_text.clone()
        } else if matches!(host.state, HostRunState::Queued | HostRunState::Connecting) {
            SharedString::default()
        } else {
            "(no output)".into()
        };
        let copy = self.output_text.clone();
        let mut pane = div().flex().flex_col().flex_1().min_w_0().min_h_0().child(
            div()
                .flex()
                .items_center()
                .px(Theme::spacing(2.))
                .py(Theme::spacing(1.))
                .child(div().flex_1().min_w_0().child(text(
                    host.heading,
                    t.subtitle2,
                    p.text.primary,
                )))
                .child(
                    IconButton::new("scripts-copy-output", "ContentCopy")
                        .icon_size(SMALL)
                        .tooltip("Copy output")
                        .disabled(empty)
                        .on_click(move |_, _, cx| {
                            cx.write_to_clipboard(ClipboardItem::new_string(copy.to_string()))
                        }),
                ),
        );
        if let Some(error) = host.error {
            pane = pane.child(
                div().px(Theme::spacing(2.)).pb(Theme::spacing(1.)).child(
                    muxus_ui::label::paragraph(error)
                        .size(t.body2.size)
                        .color(p.error.main),
                ),
            );
        }
        pane.child(
            div()
                .id("scripts-output")
                .flex_1()
                .min_h_0()
                .px(Theme::spacing(2.))
                .py(Theme::spacing(1.))
                .overflow_y_scroll()
                .track_scroll(&self.output_scroll)
                .bg(p.action.hover)
                .font_family(fonts::MONO)
                .text_size(px(12.5))
                .line_height(px(12.5 * 1.5))
                .text_color(p.text.primary)
                .child(shown),
        )
        .into_any_element()
    }
}

/// The results' counts and mode line.
struct Overview {
    running: bool,
    succeeded: usize,
    failed: usize,
    to_go: usize,
    not_run: usize,
    mode: String,
}

/// The host whose output the results show; `output` only when it changed since last frame.
struct ShownHost {
    job: u64,
    key: String,
    heading: String,
    state: HostRunState,
    error: Option<String>,
    output_len: usize,
    output: Option<String>,
}

/// `stateIcon`.
fn state_icon(state: HostRunState, theme: &Theme) -> impl IntoElement {
    let p = theme.palette;
    let (name, color): (&str, Hsla) = match state {
        HostRunState::Completed => ("CheckCircleOutlined", p.success.main),
        HostRunState::Failed => ("ErrorOutlined", p.error.main),
        HostRunState::Running | HostRunState::Connecting => ("Sync", p.primary.main),
        HostRunState::Cancelled => ("Block", p.action.disabled),
        HostRunState::Skipped => ("SkipNext", p.action.disabled),
        HostRunState::Queued => ("HourglassEmpty", p.action.disabled),
    };
    icon(name, SMALL, color)
}

impl Render for ScriptsDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let t = theme.typography;
        let job_name = self
            .jobs
            .read(cx)
            .job
            .as_ref()
            .map(|job| job.script_name.clone());
        if job_name.is_none() {
            self.view = View::Script;
        }
        let viewport = window.viewport_size();
        let (vw, vh) = (
            muxus_ui::zoom::unzoom(viewport.width),
            muxus_ui::zoom::unzoom(viewport.height),
        );

        let title = dialog_title(&theme)
            .pb(px(0.))
            .child(div().flex_1().child(text("Scripts", t.h6, p.text.primary)))
            .child(
                IconButton::new("scripts-close", "Close")
                    .tooltip("Close")
                    .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
            );
        let results_label = match &job_name {
            Some(name) => format!("Results — {name}"),
            None => "Results".into(),
        };
        let mut results_tab = Tab::new(
            "scripts-tab-results",
            results_label,
            self.view == View::Results,
        );
        if job_name.is_some() {
            results_tab = results_tab.on_click(cx.listener(|this, _, _, cx| {
                this.view = View::Results;
                cx.notify();
            }));
        }
        let tabs = tabs_row(&theme)
            .px(Theme::spacing(3.))
            .border_b_1()
            .border_color(p.divider)
            .child(
                Tab::new(
                    "scripts-tab-script",
                    "Script & hosts",
                    self.view == View::Script,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.view = View::Script;
                    cx.notify();
                })),
            )
            .child(
                div()
                    .flex()
                    .when_some_opacity(job_name.is_none())
                    .child(results_tab),
            );

        let body = match self.view {
            View::Script => self.script_view(&theme, cx),
            View::Results => self.results_view(&theme, cx),
        };
        // `maxWidth="lg" fullWidth`, `height: 720`.
        let paper = dialog_paper(&theme)
            .w(px(1200f32.min(vw - 64.)))
            .h(px(720f32.min(vh - 64.)))
            .child(title)
            .child(tabs)
            .child(body);

        let close = cx.entity().downgrade();
        let mut root = div()
            .id("scripts-dialog")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .font_family(fonts::UI)
            .child(modal(
                paper,
                Some(Rc::new(move |window: &mut Window, cx: &mut App| {
                    let _ = close.update(cx, |this, cx| this.close(window, cx));
                })),
            ));
        if let Some((ask, _)) = &self.ask {
            root = root.child(ask.clone());
        }
        root
    }
}

/// A disabled tab's look (`disabled` on `Tab`).
trait DisabledLook: Styled + Sized {
    fn when_some_opacity(self, disabled: bool) -> Self {
        if disabled { self.opacity(0.38) } else { self }
    }
}

impl<T: Styled> DisabledLook for T {}
