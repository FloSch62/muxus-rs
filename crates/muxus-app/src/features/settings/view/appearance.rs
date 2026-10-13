//! `AppearanceSection` and `LayoutSettings` of `SettingsDialog.tsx`, with
//! `CustomTerminalSchemeSettings` from its own file.

use crate::features::settings::palette::INVALID_COLOR_SWATCH;
use std::rc::Rc;

use muxus_store::prefs::layout::{
    MAX_INACTIVE_PANE_DIM_STRENGTH, MIN_INACTIVE_PANE_DIM_STRENGTH, STATUS_BAR_ITEMS,
    clamp_inactive_pane_dim_strength, clamp_interface_zoom, js_round, with_status_bar_item,
};
use muxus_store::prefs::state::{
    CommandBarPosition, FileBrowserPosition, SidebarPosition, ThemeMode,
};
use muxus_term::palette::terminal_scheme;
use muxus_ui::color::{hex, try_hex};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Focusable, IntoElement, ParentElement, SharedString, Styled, Window, div,
    px,
};
use muxus_ui::label::label;
use muxus_ui::theme::{ActiveTheme, Theme};
use muxus_ui::widgets::{Button, Checkbox};

use super::super::js::{number_to_string, to_fixed};
use super::super::texts;
use super::fields::field;
use super::kit::{
    BODY2, BODY2_LINE, ToggleItem, ToggleStyle, caption, color_well, group, page, row,
    secondary_text, slider, slider_value, snap, toggle_group,
};
use super::layout_preview::layout_preview;
use super::scheme::{custom_terminal_schemes, scheme_menu_entries, scheme_value};
use super::window::{SettingsWindow, SliderDrag};

impl SettingsWindow {
    /// A chord label for a command under the current bindings.
    pub fn chord(&self, command: &str, cx: &muxus_ui::gpui_kit::App) -> Option<String> {
        let overrides = keybinding_overrides(self.prefs(cx));
        crate::keymap::hints::chord_label(command, &overrides)
    }

    pub fn appearance(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let theme = *cx.theme();
        let prefs = self.prefs(cx).clone();
        let zoom_in = self.chord("terminal.zoom-in", cx);
        let zoom_out = self.chord("terminal.zoom-out", cx);

        let theme_mode = toggle_group(
            "theme-mode",
            &theme,
            vec![
                ToggleItem::text("Light"),
                ToggleItem::text("System"),
                ToggleItem::text("Dark"),
            ],
            Some(match prefs.theme_mode {
                ThemeMode::Light => 0,
                ThemeMode::Os => 1,
                ThemeMode::Dark => 2,
            }),
            ToggleStyle::TEXT,
            self.pref_toggle(|p, index| {
                p.theme_mode = [ThemeMode::Light, ThemeMode::Os, ThemeMode::Dark][index];
            }),
        );
        let zoom = self.row_select(
            &theme,
            "interface-zoom",
            150.,
            texts::interface_zoom_options(),
            &number_to_string(clamp_interface_zoom(prefs.interface_zoom)),
            false,
            cx,
            |this, value, _, cx| {
                if let Ok(zoom) = value.parse::<f64>() {
                    this.set_prefs(cx, |p| p.interface_zoom = zoom);
                }
            },
        );
        let window_group = group("Window")
            .row(
                row(&theme, "Theme")
                    .description_text(
                        &theme,
                        "System follows your operating system's light or dark setting.",
                    )
                    .control(theme_mode)
                    .render(),
            )
            .row(
                row(&theme, "Interface scale")
                    .description_text(
                        &theme,
                        texts::interface_scale_description(zoom_in.as_deref(), zoom_out.as_deref()),
                    )
                    .control(zoom)
                    .render(),
            )
            .render(&theme);

        let layout = self.layout_settings(&theme, &prefs);
        let status_bar = self.status_bar_group(&theme, &prefs);
        let colors = self.terminal_colors(&theme, &prefs, cx);
        let custom = self.custom_schemes_group(&theme, window, cx);
        let font = self.terminal_font(&theme, &prefs, window, cx);
        let panes = self.split_panes(&theme, &prefs);

        page(
            &theme,
            "Appearance",
            Some(secondary_text(
                &theme,
                "How Muxus and its terminals look on this machine. Changes apply to open terminals right away.",
            )),
            vec![window_group, layout, status_bar, colors, custom, font, panes],
        )
        .into_any_element()
    }

    /// A toggle group's choice written into the preferences.
    pub fn pref_toggle(
        &self,
        set: fn(&mut muxus_store::prefs::PrefsState, usize),
    ) -> super::kit::IndexHandler {
        let prefs = self.prefs.clone();
        Rc::new(move |index, _, cx| {
            prefs.update(cx, |model, cx| model.update(cx, |p| set(p, index)));
        })
    }

    /// Where the hosts sidebar, the file browser and the command bar dock,
    /// beside a live miniature.
    fn layout_settings(
        &mut self,
        theme: &Theme,
        prefs: &muxus_store::prefs::PrefsState,
    ) -> AnyElement {
        let has_command_buttons =
            !prefs.command_buttons.is_empty() || prefs.command_button_groups.len() > 1;
        let note = texts::command_bar_note(prefs.show_command_bar, has_command_buttons);
        let sidebar_left = prefs.sidebar_position == SidebarPosition::Left;
        let sidebar = toggle_group(
            "layout-sidebar",
            theme,
            vec![
                // The glyph draws its panel on the right; mirrored, it shows the left.
                ToggleItem::icon("Left", "ViewSidebarOutlined", true),
                ToggleItem::icon("Right", "ViewSidebarOutlined", false),
            ],
            Some(if sidebar_left { 0 } else { 1 }),
            ToggleStyle::LAYOUT,
            self.pref_toggle(|p, index| {
                p.sidebar_position = [SidebarPosition::Left, SidebarPosition::Right][index];
            }),
        );
        let files = toggle_group(
            "layout-files",
            theme,
            vec![
                ToggleItem::icon("Terminal", "VerticalSplitOutlined", false),
                ToggleItem::icon("Sidebar", "ViewSidebarOutlined", sidebar_left),
            ],
            Some(match prefs.file_browser_position {
                FileBrowserPosition::Pane => 0,
                FileBrowserPosition::Sidebar => 1,
            }),
            ToggleStyle::LAYOUT,
            self.pref_toggle(|p, index| {
                p.file_browser_position =
                    [FileBrowserPosition::Pane, FileBrowserPosition::Sidebar][index];
            }),
        );
        let bar = toggle_group(
            "layout-command-bar",
            theme,
            vec![
                ToggleItem::icon("Top", "VerticalAlignTop", false),
                ToggleItem::icon("Bottom", "VerticalAlignBottom", false),
            ],
            Some(match prefs.command_bar_position {
                CommandBarPosition::Top => 0,
                CommandBarPosition::Bottom => 1,
            }),
            ToggleStyle::LAYOUT,
            self.pref_toggle(|p, index| {
                p.command_bar_position =
                    [CommandBarPosition::Top, CommandBarPosition::Bottom][index];
            }),
        );
        let rows = div()
            .flex()
            .flex_col()
            .flex_grow(1.)
            .flex_shrink(1.)
            .flex_basis(px(300.))
            .min_w_0()
            .child(
                row(theme, "Hosts sidebar")
                    .description_text(theme, "The saved hosts and folders.")
                    .control(sidebar)
                    .render(),
            )
            .child(
                row(theme, "File browser")
                    .description_text(
                        theme,
                        "Beside each SSH terminal, or as a tab in the sidebar that follows the active session.",
                    )
                    .control(files)
                    .render(),
            )
            .child(
                row(theme, "Command bar")
                    .description_text(theme, note)
                    .control(bar)
                    .render(),
            );
        let body = div()
            .flex()
            .flex_wrap()
            .items_center()
            .child(div().p(px(16.)).pr(px(4.)).child(layout_preview(
                theme,
                prefs.sidebar_position,
                prefs.file_browser_position,
                prefs.command_bar_position,
            )))
            .child(rows);
        div()
            .flex()
            .flex_col()
            .child(group("Layout").flush().row(body).render(theme))
            .child(div().mt(px(8.)).child(caption(
                theme,
                "Right-click empty space in the sidebar, or the command bar itself, to move either one without opening Settings. The file browser moves with the button in its header.",
                1.66,
            )))
            .into_any_element()
    }

    fn status_bar_group(
        &mut self,
        theme: &Theme,
        prefs: &muxus_store::prefs::PrefsState,
    ) -> AnyElement {
        let shown = prefs.show_status_bar;
        let mut items = div().flex().flex_wrap().gap_x(px(16.)).mt(px(8.));
        for item in STATUS_BAR_ITEMS {
            let checked = prefs.status_bar_items.contains(&item);
            let prefs_entity = self.prefs.clone();
            items = items.child(
                div()
                    .flex()
                    .items_center()
                    .flex_grow(1.)
                    .flex_basis(px(160.))
                    .min_w(px(160.))
                    .child(
                        Checkbox::new(
                            SharedString::from(format!("status-item-{}", item.id())),
                            checked,
                        )
                        .small()
                        .disabled(!shown)
                        .on_click(move |_, _, cx| {
                            prefs_entity.update(cx, |model, cx| {
                                model.update(cx, |p| {
                                    p.status_bar_items =
                                        with_status_bar_item(&p.status_bar_items, item, !checked);
                                })
                            });
                        }),
                    )
                    .child(
                        label(item.label())
                            .size(BODY2)
                            .line_height(snap(BODY2_LINE))
                            .color(if shown {
                                theme.palette.text.primary
                            } else {
                                theme.palette.text.disabled
                            }),
                    ),
            );
        }
        group("Status bar")
            .row(self.switch_row(
                theme,
                "settings-status-bar",
                "Show status bar",
                Some("The active session's host along the bottom of the window. SSH hosts are read every few seconds with a short command over the open connection; nothing is installed on them."),
                shown,
                |p, value| p.show_status_bar = value,
            ))
            .row(
                row(theme, "Items")
                    .disabled(theme, "Items", !shown)
                    .description_text(
                        theme,
                        "Right-click the bar to change these without opening Settings.",
                    )
                    .child(items)
                    .render(),
            )
            .render(theme)
    }

    fn terminal_colors(
        &mut self,
        theme: &Theme,
        prefs: &muxus_store::prefs::PrefsState,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let custom = custom_terminal_schemes(prefs);
        let effective = if theme.mode.is_dark() {
            muxus_store::prefs::state::EffectiveThemeMode::Dark
        } else {
            muxus_store::prefs::state::EffectiveThemeMode::Light
        };
        let scheme = terminal_scheme(Some(prefs.terminal_scheme_id_for_mode(effective)), &custom);
        let scheme_foreground = scheme.theme.foreground.clone();
        let scheme_background = scheme.theme.background.clone();
        let light = self.scheme_select(
            theme,
            "light-terminal-theme",
            &prefs.light_terminal_scheme,
            cx,
            |p, id| {
                p.light_terminal_scheme = id;
            },
        );
        let dark = self.scheme_select(
            theme,
            "dark-terminal-theme",
            &prefs.dark_terminal_scheme,
            cx,
            |p, id| {
                p.dark_terminal_scheme = id;
            },
        );
        let text_color = self.color_override_row(
            theme,
            "Text color",
            texts::text_color_description(!prefs.font_color.is_empty()),
            &prefs.font_color,
            &scheme_foreground,
            "font-color",
            cx,
            |p, value| p.font_color = value,
        );
        let background = self.color_override_row(
            theme,
            "Background color",
            texts::background_color_description(!prefs.background_color.is_empty()),
            &prefs.background_color,
            &scheme_background,
            "background-color",
            cx,
            |p, value| p.background_color = value,
        );
        group("Terminal colors")
            .row(
                row(theme, "Light terminal theme")
                    .description_text(theme, "Used while the application theme is light.")
                    .control(div().w(px(280.)).child(light))
                    .render(),
            )
            .row(
                row(theme, "Dark terminal theme")
                    .description_text(theme, "Used while the application theme is dark.")
                    .control(div().w(px(280.)).child(dark))
                    .render(),
            )
            .row(text_color)
            .row(background)
            .render(theme)
    }

    /// `TerminalSchemeSelect` (medium outlined select, no floating label).
    pub fn scheme_select(
        &mut self,
        theme: &Theme,
        key: &'static str,
        value: &str,
        cx: &mut Context<Self>,
        set: fn(&mut muxus_store::prefs::PrefsState, String),
    ) -> AnyElement {
        let prefs = self.prefs(cx).clone();
        let custom = custom_terminal_schemes(&prefs);
        let scheme = terminal_scheme(Some(value), &custom).clone();
        let anchor = self.anchor(key);
        let open = self.menu_open(key);
        let entries = scheme_menu_entries(&custom, "");
        let selected = value.to_string();
        let this = cx.entity().downgrade();
        super::kit::row_select(
            SharedString::from(format!("select-{key}")),
            280.,
            scheme_value(theme, &scheme),
            open,
            false,
            &anchor,
            theme,
            Rc::new(move |_, cx| {
                let entries = entries.clone();
                let selected = selected.clone();
                let _ = this.update(cx, |this, cx| {
                    this.open_menu(
                        key,
                        280.,
                        Some(390.),
                        entries,
                        Some(selected),
                        Rc::new(move |this, id, _, cx| this.set_prefs(cx, |p| set(p, id))),
                        cx,
                    );
                });
            }),
        )
        .h(px(30. + 33.))
        .into_any_element()
    }

    #[allow(clippy::too_many_arguments)]
    fn color_override_row(
        &mut self,
        theme: &Theme,
        title: &str,
        description: &str,
        value: &str,
        scheme_color: &str,
        key: &'static str,
        cx: &mut Context<Self>,
        set: fn(&mut muxus_store::prefs::PrefsState, String),
    ) -> AnyElement {
        let shown = if value.is_empty() {
            scheme_color
        } else {
            value
        };
        let color = try_hex(shown).unwrap_or_else(|| hex(INVALID_COLOR_SWATCH));
        let mut setting =
            row(theme, title.to_string()).description_text(theme, description.to_string());
        if !value.is_empty() {
            let prefs = self.prefs.clone();
            setting = setting.control(
                Button::new(
                    SharedString::from(format!("{key}-reset")),
                    "Use scheme color",
                )
                .on_click(move |_, _, cx| {
                    prefs.update(cx, |model, cx| model.update(cx, |p| set(p, String::new())));
                }),
            );
        }
        let initial = shown.to_string();
        let this = cx.entity().downgrade();
        let anchor = self.anchor(key);
        setting
            .control(
                color_well(
                    SharedString::from(format!("{key}-well")),
                    theme,
                    color,
                    34.,
                    28.,
                    Rc::new(move |_, cx| {
                        let initial = initial.clone();
                        let _ = this.update(cx, |this, cx| {
                            this.open_color_picker(
                                key,
                                &initial,
                                Rc::new(move |this, value, cx| {
                                    this.set_prefs(cx, |p| set(p, value));
                                }),
                                cx,
                            );
                        });
                    }),
                )
                .child(muxus_ui::widgets::bounds_probe(&anchor)),
            )
            .render()
    }

    fn terminal_font(
        &mut self,
        theme: &Theme,
        prefs: &muxus_store::prefs::PrefsState,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let installed = self.state.installed_fonts(cx);
        let installed_refs: Option<Vec<&str>> = installed
            .as_ref()
            .map(|fonts| fonts.iter().map(String::as_str).collect());
        let available = muxus_term::font_catalog::terminal_font_is_available(
            &prefs.font_family,
            installed_refs.as_deref(),
        );
        let (description, error) = texts::font_family_description(
            false,
            available,
            &prefs.font_family,
            installed.is_some(),
        );
        let family = prefs.font_family.clone();
        let input = self.input(
            "font-family",
            || family.clone(),
            None,
            false,
            window,
            cx,
            |this, value, _, cx| this.set_prefs(cx, |p| p.font_family = value),
        );
        self.sync_input("font-family", &prefs.font_family, window, cx);
        let anchor = self.anchor("font-family");
        self.state.font_input_focused = input.read(cx).focus_handle(cx).is_focused(window);
        let font_field = div()
            .relative()
            .w(px(280.))
            .child(
                field("settings-font-family", &input)
                    .width(280.)
                    .error(error)
                    .render(theme, window, cx),
            )
            .child(muxus_ui::widgets::bounds_probe(&anchor));
        let description = if error {
            muxus_ui::label::paragraph(description)
                .size(super::kit::CAPTION)
                .line_height(snap(super::kit::CAPTION * 1.5))
                .color(theme.palette.error.main)
                .into_any_element()
        } else {
            muxus_ui::label::paragraph(description)
                .size(super::kit::CAPTION)
                .line_height(snap(super::kit::CAPTION * 1.5))
                .color(theme.palette.text.secondary)
                .into_any_element()
        };
        let font_size = prefs.mono_font_size;
        let line_height = prefs.line_height;
        let size_slider = self.pref_slider(
            theme,
            "font-size",
            ((font_size - 8.) / 16.) as f32,
            false,
            |p, fraction| {
                p.mono_font_size = js_round(8. + f64::from(fraction) * 16.);
            },
        );
        let height_slider = self.pref_slider(
            theme,
            "line-height",
            ((line_height - 1.) / 0.6) as f32,
            false,
            |p, fraction| {
                let steps = js_round(f64::from(fraction) * 0.6 / 0.05);
                p.line_height = ((1. + steps * 0.05) * 100.).round() / 100.;
            },
        );
        group("Terminal font")
            .row(
                row(theme, "Font family")
                    .description(description)
                    .control(font_field)
                    .render(),
            )
            .row(
                row(theme, "Font size")
                    .control(size_slider)
                    .control(slider_value(
                        theme,
                        format!("{}px", number_to_string(font_size)),
                        false,
                    ))
                    .render(),
            )
            .row(
                row(theme, "Line height")
                    .control(height_slider)
                    .control(slider_value(theme, to_fixed(line_height, 2), false))
                    .render(),
            )
            .render(theme)
    }

    /// A slider bound to a preference: a press jumps there, a drag follows.
    pub fn pref_slider(
        &mut self,
        theme: &Theme,
        key: &'static str,
        fraction: f32,
        disabled: bool,
        set: fn(&mut muxus_store::prefs::PrefsState, f32),
    ) -> AnyElement {
        let anchor = self.anchor(key);
        let prefs = self.prefs.clone();
        let entity = self.self_handle.clone();
        let drag_anchor = anchor.clone();
        slider(
            SharedString::from(format!("slider-{key}")),
            theme,
            fraction,
            disabled,
            &anchor,
            Rc::new(move |fraction, _, cx| {
                prefs.update(cx, |model, cx| model.update(cx, |p| set(p, fraction)));
                let drag_anchor = drag_anchor.clone();
                if let Some(entity) = entity.as_ref().and_then(|entity| entity.upgrade()) {
                    entity.update(cx, |this, _| {
                        this.drag = Some(SliderDrag {
                            on_move_2d: None,
                            anchor: drag_anchor,
                            on_move: Rc::new(move |this, fraction, cx| {
                                this.set_prefs(cx, |p| set(p, fraction));
                            }),
                        });
                    });
                }
            }),
        )
        .into_any_element()
    }

    fn split_panes(&mut self, theme: &Theme, prefs: &muxus_store::prefs::PrefsState) -> AnyElement {
        let dim = prefs.dim_inactive_panes;
        let strength = clamp_inactive_pane_dim_strength(prefs.inactive_pane_dim_strength);
        let range = MAX_INACTIVE_PANE_DIM_STRENGTH - MIN_INACTIVE_PANE_DIM_STRENGTH;
        let strength_slider = self.pref_slider(
            theme,
            "dim-strength",
            ((strength - MIN_INACTIVE_PANE_DIM_STRENGTH) / range) as f32,
            !dim,
            |p, fraction| {
                // Steps of 5 percentage points between the bounds.
                let min = MIN_INACTIVE_PANE_DIM_STRENGTH * 100.;
                let max = MAX_INACTIVE_PANE_DIM_STRENGTH * 100.;
                let value = min + js_round(f64::from(fraction) * (max - min) / 5.) * 5.;
                p.inactive_pane_dim_strength = value / 100.;
            },
        );
        group("Split panes")
            .row(self.switch_row(
                theme,
                "settings-dim-inactive",
                "Dim inactive panes",
                Some("Fades every pane except the focused one. Multi-exec panes stay emphasized."),
                dim,
                |p, value| p.dim_inactive_panes = value,
            ))
            .row(
                row(theme, "Dimming strength")
                    .disabled(theme, "Dimming strength", !dim)
                    .control(strength_slider)
                    .control(slider_value(
                        theme,
                        format!("{}%", js_round(strength * 100.)),
                        !dim,
                    ))
                    .render(),
            )
            .row(self.switch_row(
                theme,
                "settings-active-pane-border",
                "Outline the focused pane",
                Some("A thin accent outline around the pane that has focus."),
                prefs.active_pane_border,
                |p, value| p.active_pane_border = value,
            ))
            .render(theme)
    }
}

/// The keybinding overrides in the keymap's shape.
pub fn keybinding_overrides(
    prefs: &muxus_store::prefs::PrefsState,
) -> crate::keymap::bindings::KeybindingOverrides {
    prefs
        .keybindings
        .iter()
        .map(|(id, chords)| (id.clone(), chords.clone()))
        .collect()
}
