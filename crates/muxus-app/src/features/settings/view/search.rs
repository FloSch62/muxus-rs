//! Settings search in the window (`components/SettingsSearch.tsx`, the
//! `reveal` of `useSettingsNavigation` and the focus outline of
//! `SettingsLayout.tsx`): a search field above the section list lists results
//! as you type; choosing one opens its section, scrolls to the setting and
//! outlines it for a moment. Ctrl/Cmd+F moves to the field from anywhere in
//! Settings.
//!
//! Rows and groups learn which setting to outline the way React's context
//! tells them: the window sets a [`FocusFrame`] while it renders the section,
//! and [`kit::Row`] and [`kit::Group`] consult it.
//!
//! [`kit::Row`]: super::kit::Row
//! [`kit::Group`]: super::kit::Group

use std::cell::RefCell;
use std::time::Duration;

use muxus_ui::gpui_kit::base::input::InputState;
use muxus_ui::gpui_kit::{
    AnyElement, Context, Div, Entity, Hsla, IntoElement, KeyDownEvent, ParentElement, Styled,
    Window, div, point, px,
};
use muxus_ui::icon::icon;
use muxus_ui::theme::ActiveTheme;
use muxus_ui::widgets::form_field::FormField;
use muxus_ui::widgets::{AnchorBounds, Autocomplete, AutocompleteOption, bounds_probe};

use super::super::search::{
    EntryKind, SETTINGS_SEARCH_INDEX, SettingsFocusTarget, SettingsSearchContext,
    SettingsSearchEntry, search_settings, settings_title_key,
};
use super::super::section::SettingsTarget;
use super::window::SettingsWindow;

/// How long a picked setting stays outlined (`FOCUS_HIGHLIGHT_MS`).
pub const FOCUS_HIGHLIGHT_MS: u64 = 2500;
/// The search field's input, among the window's inputs.
const INPUT_KEY: &str = "settings-search";
/// Frames to wait for the picked setting to render (sections load their data).
const SCROLL_TRIES: u8 = 60;

/// What search keeps while the window is open.
#[derive(Default)]
pub struct SearchState {
    pub query: String,
    /// The setting picked last, while it is outlined.
    pub focus: Option<SettingsFocusTarget>,
    nonce: u64,
    /// Where the picked setting was painted.
    anchor: AnchorBounds,
    /// Frames left to bring the picked setting into view; 0 once it is there.
    scroll_tries: u8,
}

/// The setting to outline while a section renders.
struct FocusFrame {
    label: &'static str,
    kind: EntryKind,
    color: Hsla,
    anchor: AnchorBounds,
}

thread_local! {
    static FRAME: RefCell<Option<FocusFrame>> = const { RefCell::new(None) };
}

/// While alive, rows and groups rendered see `state`'s picked setting
/// (`SettingsFocusContext.Provider`); dropping it restores what was there.
pub struct FocusScope(Option<FocusFrame>);

impl Drop for FocusScope {
    fn drop(&mut self) {
        let previous = self.0.take();
        FRAME.with(|slot| *slot.borrow_mut() = previous);
    }
}

/// Outline `state`'s picked setting in what renders until the scope drops.
pub fn focus_scope(state: &SearchState, color: Hsla) -> FocusScope {
    let frame = state.focus.as_ref().map(|focus| FocusFrame {
        label: focus.label,
        kind: focus.kind,
        color,
        anchor: state.anchor.clone(),
    });
    FocusScope(FRAME.with(|slot| slot.replace(frame)))
}

/// The outline a picked setting wears: 2px of the primary color just inside
/// its edge (`outline: 2, outlineOffset: -2`), above the content.
fn outline(color: Hsla, radius: f32) -> Div {
    div()
        .absolute()
        .top_0()
        .left_0()
        .size_full()
        .border_2()
        .border_color(color)
        .rounded(px(radius))
}

/// `SettingRow`'s focus: for the row `label` names, the outline to put last
/// in it (the row must be `relative`); it also records where the row is.
pub fn row_focus(label: &str) -> Option<AnyElement> {
    FRAME.with(|slot| {
        let slot = slot.borrow();
        let frame = slot
            .as_ref()
            .filter(|frame| frame.kind == EntryKind::Row && frame.label == label)?;
        Some(
            outline(frame.color, 0.)
                .child(bounds_probe(&frame.anchor))
                .into_any_element(),
        )
    })
}

/// `SettingsGroup`'s focus: for the group titled `title`, a probe recording
/// where the whole group is (put it in the group) and the outline to put last
/// in its card (both must be `relative`).
pub fn group_focus(title: Option<&str>, radius: f32) -> Option<(AnyElement, AnyElement)> {
    FRAME.with(|slot| {
        let slot = slot.borrow();
        let frame = slot.as_ref().filter(|frame| {
            frame.kind == EntryKind::Group
                && title.is_some_and(|title| frame.label == settings_title_key(title))
        })?;
        Some((
            bounds_probe(&frame.anchor).into_any_element(),
            outline(frame.color, radius).into_any_element(),
        ))
    })
}

impl SettingsWindow {
    /// Where search runs: the desktop app, on the backend's platform.
    fn search_context(&self) -> SettingsSearchContext {
        SettingsSearchContext {
            desktop: true,
            platform: self
                .data
                .app_info
                .as_ref()
                .map(|info| info.platform.clone()),
        }
    }

    /// `SettingsSearch`: the field atop the section list.
    pub fn search_field(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        div()
            .px(px(8.))
            .pt(px(12.))
            .pb(px(6.))
            .child(self.search_autocomplete(window, cx))
            .into_any_element()
    }

    /// The search field itself: atop the section list, or above the section
    /// picker in a narrow window (one shows at a time).
    pub fn search_autocomplete(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let input = self.search_input(window, cx);
        let theme = *cx.theme();
        let query = self.state.search.query.clone();
        let results = search_settings(&query, &self.search_context());
        let options: Vec<AutocompleteOption> = results
            .iter()
            .map(|entry| {
                let place = match entry.group {
                    Some(group) => format!("{} \u{203a} {group}", entry.section.label()),
                    None => entry.section.label().to_string(),
                };
                AutocompleteOption::new(entry.label)
                    .value(entry.key())
                    .detail(place)
                    .leading(entry.section.icon())
            })
            .collect();
        let this = cx.entity().downgrade();
        let field = FormField::input("settings-search-field", &input)
            .placeholder("Search settings")
            .start(icon(
                "Search",
                muxus_ui::icon::SMALL,
                theme.palette.text.secondary,
            ));
        Autocomplete::new("settings-search", &input, field, options)
            .unfiltered()
            .auto_highlight()
            .hide_popup_indicator()
            .clear_on_pick()
            .clear_on_escape()
            .list_min_width(380.)
            .no_options_text("No settings match")
            .open_when(!query.trim().is_empty())
            .on_pick(move |option, window, cx| {
                let Some(entry) = SETTINGS_SEARCH_INDEX
                    .iter()
                    .find(|entry| entry.key() == option.value)
                else {
                    return;
                };
                let _ = this.update(cx, |this, cx| {
                    this.state.search.query.clear();
                    // `blurOnSelect`: the keyboard goes back to the window.
                    let focus = this.focus_handle_ref().clone();
                    window.focus(&focus, cx);
                    this.reveal(entry, window, cx);
                });
            })
            .into_any_element()
    }

    fn search_input(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Entity<InputState> {
        self.input(
            INPUT_KEY,
            String::new,
            // The field sets the placeholder.
            None,
            false,
            window,
            cx,
            |this, value, _, cx| {
                this.state.search.query = value;
                cx.notify();
            },
        )
    }

    /// `reveal`: open the section of a search result and bring the setting into view.
    pub fn reveal(
        &mut self,
        entry: &SettingsSearchEntry,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let search = &mut self.state.search;
        search.nonce += 1;
        let nonce = search.nonce;
        search.focus = Some(SettingsFocusTarget {
            label: entry.focus_label(),
            kind: entry.kind,
            nonce,
        });
        search.anchor = AnchorBounds::default();
        search.scroll_tries = SCROLL_TRIES;
        self.show(SettingsTarget::section(entry.section), window, cx);
        cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(FOCUS_HIGHLIGHT_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                let search = &mut this.state.search;
                if search.focus.as_ref().is_some_and(|f| f.nonce == nonce) {
                    search.focus = None;
                    search.scroll_tries = 0;
                    cx.notify();
                }
            });
        })
        .detach();
        cx.notify();
    }

    /// After a render: once the picked setting has been painted, scroll it
    /// into view (centered for a row, at the top for a group).
    pub fn follow_search_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.state.search.scroll_tries == 0 {
            return;
        }
        cx.on_next_frame(window, |this, _, cx| {
            let search = &mut this.state.search;
            let Some(focus) = search.focus.clone() else {
                search.scroll_tries = 0;
                return;
            };
            let Some(target) = search.anchor.get() else {
                // Not painted yet: its section may still be loading.
                search.scroll_tries = search.scroll_tries.saturating_sub(1);
                cx.notify();
                return;
            };
            search.scroll_tries = 0;
            let view = this.scroll.bounds();
            let offset = this.scroll.offset();
            let max = this.scroll.max_offset();
            let delta = match focus.kind {
                EntryKind::Row => {
                    (target.origin.y - view.origin.y) - (view.size.height - target.size.height) / 2.
                }
                EntryKind::Group => target.origin.y - view.origin.y,
            };
            let y = (offset.y - delta).min(px(0.)).max(-max.y);
            this.scroll.set_offset(point(offset.x, y));
            cx.notify();
        });
    }

    /// Ctrl+F (Cmd+F on macOS) moves to the search field from anywhere in Settings.
    pub fn search_shortcut(
        &mut self,
        event: &KeyDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let modifiers = event.keystroke.modifiers;
        let command = if cfg!(target_os = "macos") {
            modifiers.platform
        } else {
            modifiers.control
        };
        if !command
            || modifiers.shift
            || modifiers.alt
            || !event.keystroke.key.eq_ignore_ascii_case("f")
        {
            return false;
        }
        let Some(input) = self.input_entity(INPUT_KEY) else {
            return false;
        };
        cx.stop_propagation();
        self.menu = None;
        input.update(cx, |input, cx| {
            input.focus(window, cx);
            input.select_all(window, cx);
        });
        true
    }
}
