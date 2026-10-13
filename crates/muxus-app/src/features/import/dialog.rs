//! `components/SessionImportDialog.tsx` with the per-vendor copy of
//! `MobaXtermImportDialog.tsx` and `SecureCrtImportDialog.tsx`: choose where
//! sessions come from (a file, or this Windows account's MobaXterm), review
//! them (search, selection, storage, conflicts, skipped entries) and import
//! them through the backend.
//!
//! The model is `muxus_import::review::ImportReview`; the writes are
//! [`super::apply::import_reviewed`]. The settings' Backup & data section
//! opens it through `SettingsHooks::import_sessions`.

use std::rc::Rc;

use muxus_import::review::{
    ImportVendor, SkippedPage, session_details, session_kind_label, skipped_session_name,
};
use muxus_import::{
    ConflictStrategy, ExistingHosts, ImportError, ImportReview, ImportedSession,
    ImportedSessionKind, ImportedSshStorage, RestoreResult,
};
use muxus_session::Backend;
use muxus_ui::gpui_kit::base::input::{InputEvent, InputState};
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, ElementId, Entity, EventEmitter, FocusHandle, Focusable,
    Hsla, InteractiveElement, IntoElement, KeyDownEvent, ParentElement, PathPromptOptions, Render,
    SharedString, StatefulInteractiveElement, Styled, Subscription, Window, div, px, relative,
};
use muxus_ui::icon::{MEDIUM, icon};
use muxus_ui::label::{label, paragraph};
use muxus_ui::text::measure_text;
use muxus_ui::theme::{ActiveTheme, Status, Theme, fonts, weight};
use muxus_ui::widgets::dialog::{
    DialogWidth, dialog_actions, dialog_content, dialog_paper, dialog_title,
};
use muxus_ui::widgets::display::{AlertVariant, alert_ink};
use muxus_ui::widgets::form_control::{FormControl, FormControlLabel, form_label};
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::layer::modal_layer;
use muxus_ui::widgets::progress::CircularProgress;
use muxus_ui::widgets::{Alert, Button, ButtonColor, Checkbox, Chip};

use super::apply::{existing_hosts, import_reviewed};
use crate::features::launcher::kit::show_toast;

/// `SourceCard`'s minimum height and a session list's maximum height (`sx`).
const SOURCE_CARD_MIN_HEIGHT: f32 = 180.;
const SESSION_LIST_MAX_HEIGHT: f32 = 330.;
const SKIPPED_LIST_MAX_HEIGHT: f32 = 180.;
/// The skipped list's name column (`minmax(180px, 0.7fr) 1fr`).
const SKIPPED_NAME_MIN_WIDTH: f32 = 180.;
const SKIPPED_NAME_SHARE: f32 = 0.7;
/// Busy buttons show a 15px spinner as their start icon.
const BUSY_SPINNER: f32 = 15.;
/// A session row leaves room for the "Already exists" chip (`pr: 16`).
const CONFLICT_ROW_PADDING: f32 = 16.;
/// The filter field's search icon (`fontSize: 19`).
const SEARCH_ICON: f32 = 19.;
/// `ListItemIcon minWidth: 38` holding an `edge="start"` checkbox.
const CHECK_COLUMN: f32 = 38.;

/// What runs while the dialog waits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportBusy {
    Detect,
    File,
    Import,
}

/// What the dialog tells its owner.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SessionImportEvent {
    Close,
    /// Sessions were written: refresh the host catalog, then close.
    Imported(RestoreResult),
}

/// The vendor's icon in the title and on the local-discovery card.
pub fn vendor_icon(vendor: ImportVendor) -> &'static str {
    match vendor {
        ImportVendor::MobaXterm => "LaptopWindowsOutlined",
        ImportVendor::SecureCrt => "TerminalOutlined",
    }
}

/// The connection-kind icon of a non-SSH session's chip (`hostKindIcon`).
pub fn kind_icon(session: &ImportedSession) -> &'static str {
    match session.kind() {
        ImportedSessionKind::Serial => "UsbOutlined",
        ImportedSessionKind::Rdp => "DesktopWindowsOutlined",
        ImportedSessionKind::Vnc => "ScreenShareOutlined",
        _ => "DnsOutlined",
    }
}

/// The import button's label.
pub fn import_button_label(review: &ImportReview, busy: Option<ImportBusy>) -> String {
    if busy == Some(ImportBusy::Import) {
        "Importing…".into()
    } else {
        review.import_label()
    }
}

pub struct SessionImportDialog {
    backend: Backend,
    vendor: ImportVendor,
    review: Option<ImportReview>,
    existing: ExistingHosts,
    busy: Option<ImportBusy>,
    error: Option<String>,
    search: Entity<InputState>,
    skipped_page: usize,
    focus: FocusHandle,
    _events: Subscription,
}

impl EventEmitter<SessionImportEvent> for SessionImportDialog {}

impl Focusable for SessionImportDialog {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl SessionImportDialog {
    pub fn new(
        backend: Backend,
        vendor: ImportVendor,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx));
        let events = cx.subscribe(&search, |this, input, event: &InputEvent, cx| {
            if let InputEvent::Change = event {
                let value = input.read(cx).value().to_string();
                if let Some(review) = &mut this.review {
                    review.search = value;
                }
                cx.notify();
            }
        });
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let mut dialog = Self {
            backend,
            vendor,
            review: None,
            existing: ExistingHosts::default(),
            busy: None,
            error: None,
            search,
            skipped_page: 0,
            focus,
            _events: events,
        };
        dialog.load_existing(cx);
        dialog
    }

    /// Read the hosts Muxus has, for the "Already exists" marks.
    fn load_existing(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        cx.spawn(async move |this, cx| {
            if let Ok(existing) = existing_hosts(&backend).await {
                let _ = this.update(cx, |this, cx| {
                    this.existing = existing;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// Start reviewing parsed sessions (`review`): all selected, search cleared.
    pub fn set_review(
        &mut self,
        review: ImportReview,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.review = Some(review);
        self.skipped_page = 0;
        self.error = None;
        self.search
            .update(cx, |state, cx| state.set_value("", window, cx));
        cx.notify();
    }

    /// Review `content` as if it was read from a file called `source`.
    pub fn review_content(
        &mut self,
        content: &str,
        source: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match ImportReview::from_content(self.vendor, content, source) {
            Ok(review) => self.set_review(review, window, cx),
            Err(error) => {
                self.error = Some(error.0);
                cx.notify();
            }
        }
    }

    pub fn review(&self) -> Option<&ImportReview> {
        self.review.as_ref()
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_none() {
            cx.emit(SessionImportEvent::Close);
        }
    }

    fn back(&mut self, cx: &mut Context<Self>) {
        self.review = None;
        self.error = None;
        cx.notify();
    }

    /// Run a reading job on the backend's runtime (file reads and parsing
    /// need tokio) and review what it finds.
    fn read_with(
        &mut self,
        busy: ImportBusy,
        fallback: String,
        job: impl std::future::Future<Output = Result<ImportReview, ImportError>> + Send + 'static,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.busy = Some(busy);
        self.error = None;
        cx.notify();
        let handle = self.backend.runtime().spawn(job);
        cx.spawn_in(window, async move |this, cx| {
            let result = handle.await;
            let _ = this.update_in(cx, |this, window, cx| {
                this.busy = None;
                match result {
                    Ok(Ok(review)) => this.set_review(review, window, cx),
                    Ok(Err(error)) if !error.0.is_empty() => this.error = Some(error.0),
                    _ => this.error = Some(fallback),
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn choose_file(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let picked = crate::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        let vendor = self.vendor;
        cx.spawn_in(window, async move |this, cx| {
            let Some(paths) = picked.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let _ = this.update_in(cx, |this, window, cx| {
                this.read_with(
                    ImportBusy::File,
                    vendor.read_error(),
                    async move { ImportReview::read_file(vendor, &path).await },
                    window,
                    cx,
                );
            });
        })
        .detach();
    }

    fn detect_local(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.read_with(
            ImportBusy::Detect,
            self.vendor.detect_error(),
            ImportReview::detect_local_mobaxterm(),
            window,
            cx,
        );
    }

    fn import(&mut self, cx: &mut Context<Self>) {
        let Some(review) = self.review.clone() else {
            return;
        };
        if review.selected_count() == 0 || self.busy.is_some() {
            return;
        }
        self.busy = Some(ImportBusy::Import);
        self.error = None;
        cx.notify();
        let backend = self.backend.clone();
        let vendor = self.vendor;
        cx.spawn(async move |this, cx| {
            let result = import_reviewed(&backend, &review).await;
            let _ = this.update(cx, |this, cx| {
                this.busy = None;
                match result {
                    Ok(Some(result)) => {
                        show_toast(Status::Success, vendor.import_complete_message(&result), cx);
                        cx.emit(SessionImportEvent::Imported(result));
                    }
                    Ok(None) => {}
                    Err(error) => {
                        let message = if error.message.is_empty() {
                            vendor.import_error()
                        } else {
                            error.message
                        };
                        this.error = Some(message.clone());
                        show_toast(Status::Error, message, cx);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    fn update_review(&mut self, cx: &mut Context<Self>, change: impl FnOnce(&mut ImportReview)) {
        if let Some(review) = &mut self.review {
            change(review);
            cx.notify();
        }
    }

    fn on_key(&mut self, event: &KeyDownEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.keystroke.key == "escape" {
            cx.stop_propagation();
            self.close(cx);
        }
    }
}

/// A button showing a spinner as its start icon while its job runs
/// (`startIcon={<CircularProgress size={15} color="inherit" />}`).
fn busy_button(theme: &Theme, button: Button, spinner_id: &'static str, text: &str) -> Button {
    let ink = theme.palette.action.disabled;
    let button_type = theme.typography.button;
    let size = theme.typography.px_to_rem(13.);
    button.label_element(
        div()
            .flex()
            .items_center()
            .gap(Theme::spacing(1.))
            .child(CircularProgress::new(spinner_id, BUSY_SPINNER, ink))
            .child(
                label(text.to_owned())
                    .size(size)
                    .weight(button_type.weight)
                    .line_height(size * button_type.line_height / button_type.size)
                    .color(ink)
                    .no_truncate(),
            ),
    )
}

/// `SourceCard`: an outlined paper with an icon, a title, a description and
/// the card's button.
fn source_card(
    theme: &Theme,
    glyph: &'static str,
    glyph_color: Hsla,
    title: String,
    description: &'static str,
    action: AnyElement,
) -> AnyElement {
    let p = theme.palette;
    let typography = theme.typography;
    div()
        .flex()
        .flex_col()
        .flex_1()
        .min_w_0()
        .min_h(px(SOURCE_CARD_MIN_HEIGHT))
        .p(Theme::spacing(2.))
        .rounded(px(theme.radius))
        .border_1()
        .border_color(p.divider)
        .bg(p.background_paper)
        .child(div().flex().child(icon(glyph, MEDIUM, glyph_color)))
        .child(
            div().mt(Theme::spacing(1.5)).child(
                label(title)
                    .variant(typography.subtitle1)
                    .weight(weight::BOLD)
                    .color(p.text.primary),
            ),
        )
        .child(
            div()
                .mt(Theme::spacing(0.5))
                .mb(Theme::spacing(2.))
                .flex_1()
                .child(
                    paragraph(description)
                        .variant(typography.body2)
                        .color(p.text.secondary),
                ),
        )
        .child(div().flex().child(action))
        .into_any_element()
}

impl SessionImportDialog {
    fn render_idle(&self, theme: &Theme, cx: &mut Context<Self>) -> AnyElement {
        let p = theme.palette;
        let typography = theme.typography;
        let entity = cx.entity().downgrade();
        let mut cards = div().flex().gap(Theme::spacing(1.5));
        if let Some(auto) = self.vendor.auto_source() {
            let detect = entity.clone();
            let mut button = Button::new("import-detect", "Find sessions")
                .contained()
                .disabled(self.busy.is_some())
                .on_click(move |_, window, cx| {
                    let _ = detect.update(cx, |this, cx| this.detect_local(window, cx));
                });
            button = if self.busy == Some(ImportBusy::Detect) {
                busy_button(theme, button, "import-detect-spinner", "Looking…")
            } else {
                button.start_icon("SearchOutlined")
            };
            cards = cards.child(source_card(
                theme,
                vendor_icon(self.vendor),
                p.primary.main,
                auto.title.to_owned(),
                auto.description,
                button.into_any_element(),
            ));
        }
        let choose = entity;
        let mut file_button = Button::new("import-choose-file", "Choose file")
            .outlined()
            .disabled(self.busy.is_some())
            .on_click(move |_, window, cx| {
                let _ = choose.update(cx, |this, cx| this.choose_file(window, cx));
            });
        file_button = if self.busy == Some(ImportBusy::File) {
            busy_button(theme, file_button, "import-file-spinner", "Choose file")
        } else {
            file_button.start_icon("UploadFileOutlined")
        };
        cards = cards.child(source_card(
            theme,
            "UploadFileOutlined",
            p.secondary.main,
            self.vendor.file_title(),
            self.vendor.file_description(),
            file_button.into_any_element(),
        ));
        div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.))
            .child(
                paragraph(self.vendor.source_intro())
                    .variant(typography.body2)
                    .color(p.text.secondary),
            )
            .child(cards)
            .child(info_alert(theme, self.vendor.privacy_notice()))
            .into_any_element()
    }

    fn render_skipped(
        &self,
        theme: &Theme,
        review: &ImportReview,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let notice = review.skipped_notice()?;
        let typography = theme.typography;
        let ink = alert_ink(theme, Status::Warning, AlertVariant::Standard);
        let border = theme.palette.warning.dark;
        let page: SkippedPage<'_> = review.skipped_page(self.skipped_page);
        // The list is a shrink-to-fit grid (`minmax(180px, 0.7fr) 1fr`): one
        // fr is the widest reason, the name column 0.7 of it but at least 180px.
        let body2 = typography.body2;
        let reason_width = page
            .sessions
            .iter()
            .map(|session| {
                measure_text(window, &session.reason, fonts::UI, body2.size, body2.weight)
            })
            .fold(0., f32::max)
            .ceil();
        let name_width = (SKIPPED_NAME_SHARE * reason_width).max(SKIPPED_NAME_MIN_WIDTH);
        let column_gap = Theme::spacing(2.);
        let list_width = name_width + muxus_ui::zoom::unzoom(column_gap) + reason_width;
        let mut list = div()
            .id("import-skipped")
            .flex()
            .flex_col()
            .mt(Theme::spacing(1.))
            .max_h(px(SKIPPED_LIST_MAX_HEIGHT))
            .overflow_y_scroll();
        for session in page.sessions {
            list = list.child(
                div()
                    .flex()
                    .w(px(list_width))
                    .max_w(relative(1.))
                    .gap(column_gap)
                    .py(Theme::spacing(0.75))
                    .border_t_1()
                    .border_color(border)
                    .child(
                        div().w(px(name_width)).flex_none().child(
                            label(skipped_session_name(session))
                                .variant(typography.body2)
                                .weight(weight::STRONG)
                                .color(ink),
                        ),
                    )
                    .child(
                        div().flex_1().min_w_0().child(
                            paragraph(session.reason.clone())
                                .variant(typography.body2)
                                .color(ink),
                        ),
                    ),
            );
        }
        let mut content = div()
            .flex()
            .flex_col()
            .child(paragraph(notice).variant(typography.body2).color(ink))
            .child(list);
        if page.has_pager() {
            let entity = cx.entity().downgrade();
            let (previous, next) = (entity.clone(), entity);
            let current = page.page;
            content = content.child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mt(Theme::spacing(0.5))
                    .child(label(page.label()).variant(typography.caption).color(ink))
                    .child(
                        div()
                            .flex()
                            .gap(Theme::spacing(0.5))
                            .child(
                                Button::new("import-skipped-previous", "Previous")
                                    .color(ButtonColor::Inherit)
                                    .disabled(!page.has_previous())
                                    .on_click(move |_, _, cx| {
                                        let _ = previous.update(cx, |this, cx| {
                                            this.skipped_page = current.saturating_sub(1);
                                            cx.notify();
                                        });
                                    }),
                            )
                            .child(
                                Button::new("import-skipped-next", "Next")
                                    .color(ButtonColor::Inherit)
                                    .disabled(!page.has_next())
                                    .on_click(move |_, _, cx| {
                                        let _ = next.update(cx, |this, cx| {
                                            this.skipped_page = current + 1;
                                            cx.notify();
                                        });
                                    }),
                            ),
                    ),
            );
        }
        Some(
            Alert::new(Status::Warning, "")
                .content(content)
                .into_any_element(),
        )
    }

    fn radio<F>(
        &self,
        id: &'static str,
        checked: bool,
        content: impl Into<SharedString>,
        cx: &mut Context<Self>,
        pick: F,
    ) -> AnyElement
    where
        F: Fn(&mut ImportReview) + 'static,
    {
        let entity = cx.entity().downgrade();
        FormControlLabel::new(id, FormControl::Radio(checked), content)
            .on_click(move |_, _, cx| {
                let _ = entity.update(cx, |this, cx| this.update_review(cx, |review| pick(review)));
            })
            .into_any_element()
    }

    fn render_session(
        &self,
        theme: &Theme,
        review: &ImportReview,
        index: usize,
        session: &ImportedSession,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let typography = theme.typography;
        let conflict = review.is_conflict(session, &self.existing);
        let id = session.id().to_owned();
        let entity = cx.entity().downgrade();
        let toggle_id = id.clone();
        let mut primary = div()
            .flex()
            .items_center()
            .gap(Theme::spacing(1.))
            .min_w_0()
            .child(
                label(session.name().to_owned())
                    .variant(typography.body2)
                    .weight(weight::STRONG)
                    .color(p.text.primary),
            );
        if let Some(kind) = session_kind_label(session) {
            primary = primary.child(Chip::new(kind).outlined().icon(kind_icon(session)));
        }
        if let Some(folder) = session.folder() {
            primary = primary.child(
                Chip::new(folder.to_owned())
                    .outlined()
                    .icon("FolderOutlined"),
            );
        }
        let checkbox = Checkbox::new(
            SharedString::from(format!("import-check-{id}")),
            review.is_selected(&id),
        )
        .on_click(move |_, _, cx| {
            let id = toggle_id.clone();
            let _ = entity.update(cx, |this, cx| {
                this.update_review(cx, |review| review.toggle(&id))
            });
        });
        let mut row = div()
            .id(ElementId::Name(format!("import-session-{id}").into()))
            .relative()
            .flex()
            .items_center()
            .w(relative(1.))
            .pl(Theme::spacing(2.))
            .pr(if conflict {
                Theme::spacing(CONFLICT_ROW_PADDING)
            } else {
                Theme::spacing(2.)
            })
            .py(Theme::spacing(1.))
            .child(
                div()
                    .flex()
                    .flex_none()
                    .min_w(px(CHECK_COLUMN))
                    // `edge="start"`: the checkbox's padding hangs into the gutter.
                    .child(div().ml(-Theme::spacing(1.5)).child(checkbox)),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    // `ListItemText` with a secondary line: 6px above and below.
                    .my(Theme::spacing(0.75))
                    .child(primary)
                    .child(
                        paragraph(session_details(session))
                            .variant(typography.body2)
                            .color(p.text.secondary),
                    ),
            );
        if conflict {
            row = row.child(
                div()
                    .absolute()
                    .right(Theme::spacing(2.))
                    .top_0()
                    .bottom_0()
                    .flex()
                    .items_center()
                    .child(Chip::new("Already exists").palette(p.warning)),
            );
        }
        let mut item = div().flex().flex_col();
        if index > 0 {
            item = item.child(muxus_ui::widgets::display::divider(theme));
        }
        item.child(row).into_any_element()
    }

    fn render_review(
        &self,
        theme: &Theme,
        review: &ImportReview,
        window: &Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let typography = theme.typography;
        let mut stack = div()
            .flex()
            .flex_col()
            .gap(Theme::spacing(2.))
            .child(info_alert(theme, self.vendor.review_notice()));
        if let Some(skipped) = self.render_skipped(theme, review, window, cx) {
            stack = stack.child(skipped);
        }
        if review.has_ssh_hosts() {
            let muxus = review.ssh_storage == ImportedSshStorage::Muxus;
            stack = stack.child(
                div()
                    .flex()
                    .flex_col()
                    .child(form_label("Store imported SSH hosts in", cx))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .child(self.radio(
                                "import-storage-muxus",
                                muxus,
                                "Muxus app data only",
                                cx,
                                |review| review.ssh_storage = ImportedSshStorage::Muxus,
                            ))
                            .child(self.radio(
                                "import-storage-openssh",
                                !muxus,
                                "OpenSSH config",
                                cx,
                                |review| review.ssh_storage = ImportedSshStorage::Openssh,
                            )),
                    )
                    .child(
                        label(review.storage_caption())
                            .variant(typography.caption)
                            .color(p.text.secondary)
                            .no_truncate(),
                    ),
            );
        }
        let toggle_all = cx.entity().downgrade();
        stack = stack.child(
            div()
                .flex()
                .items_center()
                .gap(Theme::spacing(1.5))
                .child(
                    div().flex_1().min_w_0().child(
                        FormField::input("import-search", &self.search)
                            .placeholder("Filter sessions")
                            .start(icon("SearchOutlined", SEARCH_ICON, p.text.secondary)),
                    ),
                )
                .child(
                    Button::new("import-toggle-all", review.toggle_all_label()).on_click(
                        move |_, _, cx| {
                            let _ = toggle_all.update(cx, |this, cx| {
                                this.update_review(cx, ImportReview::toggle_all)
                            });
                        },
                    ),
                ),
        );
        let filtered: Vec<ImportedSession> =
            review.filtered_sessions().into_iter().cloned().collect();
        let mut list = div()
            .id("import-sessions")
            .flex()
            .flex_col()
            .max_h(px(SESSION_LIST_MAX_HEIGHT))
            .overflow_y_scroll()
            .rounded(px(theme.radius))
            .border_1()
            .border_color(p.divider)
            .bg(p.background_paper);
        for (index, session) in filtered.iter().enumerate() {
            let row = self.render_session(theme, review, index, session, cx);
            list = list.child(row);
        }
        if filtered.is_empty() {
            list = list.child(
                div().p(Theme::spacing(3.)).flex().justify_center().child(
                    label("No sessions match that filter.")
                        .variant(typography.body2)
                        .color(p.text.secondary)
                        .no_truncate(),
                ),
            );
        }
        stack = stack.child(list);
        if review.conflicting_count(&self.existing) > 0 {
            let keep = review.conflicts == ConflictStrategy::Keep;
            let keep_label = review.keep_label(&self.existing);
            stack = stack.child(
                div()
                    .flex()
                    .flex_col()
                    .child(form_label(
                        "When a Muxus host already matches an imported session",
                        cx,
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_wrap()
                            .child(self.radio(
                                "import-conflict-keep",
                                keep,
                                keep_label,
                                cx,
                                |review| review.conflicts = ConflictStrategy::Keep,
                            ))
                            .child(self.radio(
                                "import-conflict-replace",
                                !keep,
                                "Replace existing",
                                cx,
                                |review| review.conflicts = ConflictStrategy::Replace,
                            )),
                    ),
            );
        }
        stack.into_any_element()
    }
}

/// A standard info `Alert` holding a paragraph.
fn info_alert(theme: &Theme, text: &'static str) -> AnyElement {
    Alert::new(Status::Info, "")
        .content(
            paragraph(text)
                .variant(theme.typography.body2)
                .color(alert_ink(theme, Status::Info, AlertVariant::Standard)),
        )
        .into_any_element()
}

impl Render for SessionImportDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let p = theme.palette;
        let typography = theme.typography;
        let entity = cx.entity().downgrade();
        let review = self.review.clone();
        let busy = self.busy.is_some();

        // DialogTitle: the Back button (or the vendor icon) beside the title block.
        let leading: AnyElement = if review.is_some() {
            let back = entity.clone();
            Button::new("import-back", "Back")
                .start_icon("ArrowBackOutlined")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = back.update(cx, |this, cx| this.back(cx));
                })
                .into_any_element()
        } else {
            icon(vendor_icon(self.vendor), MEDIUM, p.primary.main).into_any_element()
        };
        let subtitle = match &review {
            Some(review) => review.subtitle(),
            None => self.vendor.idle_subtitle().to_owned(),
        };
        let title = dialog_title(&theme)
            .gap(Theme::spacing(1.25))
            .pb(Theme::spacing(1.))
            .child(div().flex().flex_none().child(leading))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .min_w_0()
                    .child(
                        label(format!("Import from {}", self.vendor.name()))
                            .variant(typography.h6)
                            .weight(weight::BOLD)
                            .color(p.text.primary),
                    )
                    // The caption is an inline span in the title's block, so its line
                    // takes the h6 strut.
                    .child(
                        label(subtitle)
                            .variant(typography.caption)
                            .line_height(typography.h6.line_height)
                            .strut(typography.h6.size)
                            .color(p.text.secondary),
                    ),
            );

        let body = match &review {
            Some(review) => self.render_review(&theme, review, window, cx),
            None => self.render_idle(&theme, cx),
        };
        let mut content = dialog_content(&theme, true, true)
            .id("import-content")
            .overflow_y_scroll()
            .child(body);
        if let Some(error) = &self.error {
            content = content.child(
                div().mt(Theme::spacing(2.)).child(
                    Alert::new(Status::Error, "").content(
                        paragraph(error.clone())
                            .variant(typography.body2)
                            .color(alert_ink(&theme, Status::Error, AlertVariant::Standard)),
                    ),
                ),
            );
        }

        let cancel = entity.clone();
        let mut actions = dialog_actions().child(
            Button::new("import-cancel", "Cancel")
                .disabled(busy)
                .on_click(move |_, _, cx| {
                    let _ = cancel.update(cx, |this, cx| this.close(cx));
                }),
        );
        if let Some(review) = &review {
            let import = entity.clone();
            let mut button = Button::new("import-run", import_button_label(review, self.busy))
                .contained()
                .disabled(busy || review.selected_count() == 0)
                .on_click(move |_, _, cx| {
                    let _ = import.update(cx, |this, cx| this.import(cx));
                });
            button = if self.busy == Some(ImportBusy::Import) {
                busy_button(&theme, button, "import-run-spinner", "Importing…")
            } else {
                button.start_icon("DnsOutlined")
            };
            actions = actions.child(button);
        }

        let viewport = window.viewport_size();
        let margin = Theme::spacing(4.);
        let width = DialogWidth::Md
            .px()
            .min(muxus_ui::zoom::unzoom(viewport.width) - 2. * f32::from(margin));
        let paper = dialog_paper(&theme)
            .w(px(width))
            .max_h(viewport.height - margin * 2.)
            .child(title)
            .child(content)
            .child(actions);
        let backdrop = entity;
        div()
            .id("session-import-dialog")
            .track_focus(&self.focus)
            .capture_key_down(cx.listener(Self::on_key))
            .child(modal_layer(
                window,
                "session-import-backdrop",
                paper,
                Some(Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = backdrop.update(cx, |this, cx| this.close(cx));
                })),
            ))
    }
}

/// Open the import dialog for a vendor; the owner drops it on
/// [`SessionImportEvent::Close`] or [`SessionImportEvent::Imported`].
pub fn open_session_import(
    backend: Backend,
    vendor: ImportVendor,
    window: &mut Window,
    cx: &mut App,
) -> Entity<SessionImportDialog> {
    cx.new(|cx| SessionImportDialog::new(backend, vendor, window, cx))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_the_import_button_and_icons() {
        let review = ImportReview::from_content(
            ImportVendor::SecureCrt,
            r#"<?xml version="1.0"?><VanDyke version="3.0"><key name="Sessions"><key name="core"><dword name="Is Session">1</dword><string name="Protocol Name">SSH2</string><string name="Hostname">core.example.com</string><dword name="[SSH2] Port">22</dword><string name="Username">admin</string></key></key></VanDyke>"#,
            "export.xml",
        )
        .unwrap();
        assert_eq!(import_button_label(&review, None), "Import 1 session");
        assert_eq!(
            import_button_label(&review, Some(ImportBusy::Import)),
            "Importing…"
        );
        assert_eq!(vendor_icon(ImportVendor::SecureCrt), "TerminalOutlined");
        assert_eq!(kind_icon(&review.sessions()[0]), "DnsOutlined");
    }
}
