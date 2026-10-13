//! `client/src/components/OpenWithDialog.tsx`: "Open with" on Linux, where the
//! desktop has no chooser Muxus can call: the installed programs, those that
//! handle the file's type first (issue #221).

use std::sync::Arc;

use muxus_core::api::LocalOpenTarget;
use muxus_ui::gpui_kit::base::input::{Input, InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, AppContext, ClickEvent, Context, ElementId, Entity, EventEmitter, FocusHandle,
    Focusable, FontWeight, InteractiveElement, IntoElement, KeyDownEvent, ParentElement,
    PathPromptOptions, Render, ScrollHandle, StatefulInteractiveElement, Styled, Subscription,
    Window, div, px, relative,
};
use muxus_ui::icon::{SMALL, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{ActiveTheme, Theme, fonts};
use muxus_ui::widgets::Button;
use muxus_ui::widgets::dialog::dialog_paper;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::text_field::style_input;
use std::rc::Rc;

use super::linux_apps::{ApplicationChoice, LinuxApplicationCatalog};
use super::services::notify;
use muxus_ui::widgets::progress::CircularProgress;

/// The dialog's answer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum OpenWithEvent {
    Open(LocalOpenTarget),
    Close,
}

/// Picked executables are named by this prefix and their path.
pub const PICKED_PREFIX: &str = "picked:";

/// Programs whose name holds the query, in the catalog's order (`visible`).
pub fn filter_choices<'a>(
    choices: &'a [ApplicationChoice],
    query: &str,
) -> Vec<&'a ApplicationChoice> {
    let needle = query.trim().to_lowercase();
    choices
        .iter()
        .filter(|choice| {
            needle.is_empty() || choice.application.name.to_lowercase().contains(&needle)
        })
        .collect()
}

/// The selection a list starts with: the first recommended program, or the
/// first match while searching.
pub fn initial_selection(visible: &[&ApplicationChoice], query: &str) -> Option<String> {
    if query.trim().is_empty() {
        visible
            .iter()
            .find(|choice| choice.recommended)
            .map(|choice| choice.application.id.clone())
    } else {
        visible.first().map(|choice| choice.application.id.clone())
    }
}

pub struct OpenWithDialog {
    file_name: String,
    choices: Option<Arc<Vec<ApplicationChoice>>>,
    failed: bool,
    query: Entity<InputState>,
    selected: Option<String>,
    scroll: ScrollHandle,
    focus: FocusHandle,
    _events: Subscription,
}

impl EventEmitter<OpenWithEvent> for OpenWithDialog {}

impl Focusable for OpenWithDialog {
    fn focus_handle(&self, _: &muxus_ui::gpui_kit::App) -> FocusHandle {
        self.focus.clone()
    }
}

impl OpenWithDialog {
    pub fn new(file_name: String, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let query = cx.new(|cx| InputState::new(window, cx).placeholder("Search programs"));
        let events = cx.subscribe_in(&query, window, |this, _, event, _, cx| match event {
            InputEvent::Change => {
                this.reselect(cx);
                cx.notify();
            }
            InputEvent::PressEnter { .. } => this.open(None, cx),
            _ => {}
        });
        let focus = query.read(cx).focus_handle(cx);
        window.focus(&focus, cx);
        let name = file_name.clone();
        cx.spawn(async move |this, cx| {
            let choices = cx
                .background_executor()
                .spawn(async move {
                    let env: std::collections::HashMap<String, String> = std::env::vars().collect();
                    let home = std::env::var_os("HOME")
                        .map(std::path::PathBuf::from)
                        .unwrap_or_default();
                    LinuxApplicationCatalog::read(&env, &home).applications_for(&name)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.choices = Some(Arc::new(choices));
                this.reselect(cx);
                cx.notify();
            });
        })
        .detach();
        Self {
            file_name,
            choices: None,
            failed: false,
            query,
            selected: None,
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            _events: events,
        }
    }

    fn query_text(&self, cx: &muxus_ui::gpui_kit::App) -> String {
        self.query.read(cx).value().to_string()
    }

    fn reselect(&mut self, cx: &mut Context<Self>) {
        let Some(choices) = self.choices.clone() else {
            return;
        };
        let query = self.query_text(cx);
        let visible = filter_choices(&choices, &query);
        if self
            .selected
            .as_ref()
            .is_some_and(|id| visible.iter().any(|c| &c.application.id == id))
        {
            return;
        }
        self.selected = initial_selection(&visible, &query);
    }

    fn open(&mut self, id: Option<String>, cx: &mut Context<Self>) {
        if let Some(id) = id.or_else(|| self.selected.clone()) {
            cx.emit(OpenWithEvent::Open(LocalOpenTarget::Application { id }));
        }
    }

    fn move_selection(&mut self, direction: isize, cx: &mut Context<Self>) {
        let Some(choices) = self.choices.clone() else {
            return;
        };
        let query = self.query_text(cx);
        let visible = filter_choices(&choices, &query);
        if visible.is_empty() {
            return;
        }
        let index = self
            .selected
            .as_ref()
            .and_then(|id| visible.iter().position(|c| &c.application.id == id));
        let next = match index {
            None => 0,
            Some(index) => {
                (index as isize + direction).clamp(0, visible.len() as isize - 1) as usize
            }
        };
        self.selected = Some(visible[next].application.id.clone());
        self.scroll.scroll_to_item(next);
        cx.notify();
    }

    /// `Browse…`: any executable, picked with the native file picker.
    fn browse(&mut self, cx: &mut Context<Self>) {
        let picked = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        cx.spawn(async move |this, cx| {
            let Some(paths) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update(cx, |_, cx| {
                let name = path
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if super::linux_apps::find_program(&path.to_string_lossy(), "").is_none() {
                    notify(
                        muxus_ui::theme::Status::Error,
                        format!("{name} is not an executable program."),
                        cx,
                    );
                    return;
                }
                cx.emit(OpenWithEvent::Open(LocalOpenTarget::Application {
                    id: format!("{PICKED_PREFIX}{}", path.display()),
                }));
            });
        })
        .detach();
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "down" => {
                cx.stop_propagation();
                self.move_selection(1, cx);
            }
            "up" => {
                cx.stop_propagation();
                self.move_selection(-1, cx);
            }
            "escape" => {
                cx.stop_propagation();
                cx.emit(OpenWithEvent::Close);
            }
            _ => {}
        }
    }

    fn render_list(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let body2 = theme.typography.body2;
        let Some(choices) = self.choices.clone() else {
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .child(CircularProgress::new(
                    "open-with-loading",
                    22.,
                    p.primary.main,
                ))
                .into_any_element();
        };
        let query = self.query_text(cx);
        let visible = filter_choices(&choices, &query);
        if visible.is_empty() {
            let trimmed = query.trim();
            let message = if self.failed {
                "The installed programs could not be listed. Use Browse… to pick one.".to_string()
            } else if !trimmed.is_empty() {
                format!("No programs match “{trimmed}”.")
            } else {
                "No programs were found. Use Browse… to pick one.".to_string()
            };
            return div()
                .size_full()
                .flex()
                .items_center()
                .justify_center()
                .px(px(24.))
                .child(
                    paragraph(message)
                        .size(body2.size)
                        .line_height(body2.line_height)
                        .color(p.text.secondary)
                        .center(),
                )
                .into_any_element();
        }
        let first_other = visible.iter().position(|c| !c.recommended);
        let subheader = |text: &'static str| {
            div()
                .flex()
                .items_center()
                .h(px(30.))
                .px(px(16.))
                .bg(p.background_paper)
                .child(
                    label(text)
                        .size(11.5)
                        .weight(500.)
                        .line_height(30.)
                        .color(p.text.secondary),
                )
        };
        let mut list = div()
            .id("open-with-programs")
            .size_full()
            .overflow_y_scroll()
            .track_scroll(&self.scroll);
        for (index, choice) in visible.iter().enumerate() {
            if index == 0 && choice.recommended {
                list = list.child(subheader("Recommended"));
            }
            if Some(index) == first_other {
                list = list.child(subheader(if index == 0 {
                    "Programs"
                } else {
                    "Other programs"
                }));
            }
            let id = choice.application.id.clone();
            let selected = self.selected.as_deref() == Some(id.as_str());
            let hover = if selected {
                theme.components.selected_pill_hover
            } else {
                theme.components.list_hover
            };
            let click_id = id.clone();
            let mut item = div()
                .id(ElementId::Name(format!("open-with-{id}").into()))
                .flex()
                .items_center()
                .mx(px(6.))
                .my(px(1.))
                .px(px(16.))
                .py(px(3.2))
                .rounded(px(6.))
                .cursor_pointer()
                .hover(move |s| s.bg(hover))
                .on_click(cx.listener(move |this, event: &ClickEvent, _, cx| {
                    this.selected = Some(click_id.clone());
                    if event.click_count() >= 2 {
                        this.open(Some(click_id.clone()), cx);
                    }
                    cx.notify();
                }))
                .child(div().flex_none().min_w(px(36.)).child(icon(
                    "AppsOutlined",
                    22.,
                    p.text.secondary,
                )));
            let mut text = div().flex().flex_col().flex_1().min_w_0().child(
                label(choice.application.name.clone())
                    .size(13.)
                    .weight(if selected { 600. } else { 400. })
                    .line_height(13. * 1.43)
                    .color(p.text.primary),
            );
            if choice.is_default {
                text = text.child(
                    label("Default")
                        .size(11.)
                        .line_height(11. * 1.43)
                        .color(p.text.secondary),
                );
            }
            item = item.child(text);
            if selected {
                item = item.bg(theme.components.selected_pill);
            }
            list = list.child(item);
        }
        list.into_any_element()
    }
}

impl Render for OpenWithDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let h6 = theme.typography.h6;
        let body1 = 14.857_143_f32;
        style_input(&self.query, &theme, cx);
        let viewport = window.viewport_size();
        let focused = self.query.read(cx).focus_handle(cx).is_focused(window);
        let search = div()
            .relative()
            .flex()
            .items_center()
            .h(px(body1 * 1.4375 + 17.))
            .pl(px(14.))
            .pr(px(14.))
            .font_family(fonts::UI)
            .text_size(px(body1))
            .line_height(px(body1 * 1.4375))
            .text_color(p.text.primary)
            .child(
                div()
                    .mr(px(8.))
                    .child(icon("Search", SMALL, p.action.active)),
            )
            .child(div().flex_1().min_w_0().child(Input::new(&self.query)))
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(px(8.))
                    .border(px(if focused { 2. } else { 1. }))
                    .border_color(if focused { p.primary.main } else { p.divider }),
            );
        let has_selection = self.selected.is_some();
        let paper = dialog_paper(&theme)
            .w(relative(1.))
            .child(
                div().px(px(24.)).pt(px(16.)).pb(px(4.)).child(
                    label("Open with")
                        .size(h6.size)
                        .weight(h6.weight)
                        .line_height(h6.line_height)
                        .tracking(-0.2)
                        .color(p.text.primary),
                ),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .px(px(24.))
                    .child(
                        div().mb(px(10.)).child(
                            label(self.file_name.clone())
                                .family(fonts::MONO)
                                .size(12.)
                                .line_height(12. * 1.43)
                                .color(p.text.secondary),
                        ),
                    )
                    .child(search)
                    .child(
                        div()
                            .mt(px(8.))
                            .h(px(340.))
                            .border_1()
                            .border_color(p.divider)
                            .rounded(px(8.))
                            .overflow_hidden()
                            .child(self.render_list(&theme, cx)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .p(px(8.))
                    .child(
                        Button::new("open-with-browse", "Browse…")
                            .on_click(cx.listener(|this, _, _, cx| this.browse(cx))),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("open-with-cancel", "Cancel")
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(OpenWithEvent::Close))),
                    )
                    .child(
                        Button::new("open-with-open", "Open")
                            .contained()
                            .disabled(!has_selection)
                            .on_click(cx.listener(|this, _, _, cx| this.open(None, cx))),
                    ),
            );
        let entity = cx.entity().downgrade();
        let close: muxus_ui::widgets::dialog::CloseHandler = Rc::new(move |_, cx| {
            let _ = entity.update(cx, |_, cx| cx.emit(OpenWithEvent::Close));
        });
        modal_layer(
            window,
            "open-with-backdrop",
            div()
                .id("open-with-dialog")
                .key_context("OpenWithDialog")
                .capture_key_down(cx.listener(Self::on_key))
                .m(px(32.))
                .w((viewport.width - px(64.)).min(px(444.)))
                .font_weight(FontWeight(400.))
                .child(paper),
            Some(close),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::super::linux_apps::DesktopApplication;
    use super::*;

    fn choice(id: &str, name: &str, recommended: bool) -> ApplicationChoice {
        ApplicationChoice {
            application: DesktopApplication {
                id: id.into(),
                name: name.into(),
                ..DesktopApplication::default()
            },
            recommended,
            is_default: false,
        }
    }

    #[test]
    fn starts_on_the_first_recommended_program_or_the_first_match() {
        let choices = vec![
            choice("a", "Alpha", false),
            choice("draw", "draw.io", true),
            choice("edit", "Text Editor", false),
        ];
        let visible = filter_choices(&choices, "");
        assert_eq!(initial_selection(&visible, "").as_deref(), Some("draw"));
        let visible = filter_choices(&choices, " EDIT ");
        assert_eq!(visible.len(), 1);
        assert_eq!(
            initial_selection(&visible, " EDIT ").as_deref(),
            Some("edit")
        );
        assert!(filter_choices(&choices, "zzz").is_empty());
    }
}
