//! `components/CustomTerminalSchemeSettings.tsx`: the user's own terminal
//! color schemes — create from a copy, edit, import, export and delete.

use crate::features::settings::palette::MISSING_SCHEME_COLOR;
use std::rc::Rc;

use muxus_term::custom_schemes::{
    CustomSchemeColors, CustomTerminalScheme, MAX_CUSTOM_TERMINAL_SCHEMES,
    MAX_TERMINAL_SCHEME_FILE_BYTES, MAX_TERMINAL_SCHEME_NAME_LENGTH, SCHEME_ANSI_COLOR_KEYS,
    SCHEME_BASE_COLOR_KEYS, SCHEME_BRIGHT_ANSI_COLOR_KEYS, create_terminal_scheme_document,
    custom_scheme_copy_of, merge_custom_terminal_schemes, parse_terminal_scheme_file,
    terminal_scheme_from_custom,
};
use muxus_term::palette::terminal_scheme;
use muxus_ui::color::{hex, try_hex};
use muxus_ui::gpui_kit::{
    AnyElement, Context, Hsla, IntoElement, ParentElement, SharedString, Styled, Window, div, px,
};
use muxus_ui::label::{label, paragraph};
use muxus_ui::theme::{Status, Theme};
use muxus_ui::widgets::{Button, ButtonColor, bounds_probe};

use super::super::files::export_filename;
use super::super::texts;
use super::fields::field;
use super::kit::{BODY2, CAPTION, color_well, group, row, secondary_text, snap};
use super::scheme::{
    custom_terminal_schemes, scheme_label, scheme_menu_entries, store_custom_schemes,
    term_custom_schemes,
};
use super::window::{MenuEntry, SettingsWindow};

const BASE_LABELS: [&str; 4] = ["Background", "Text", "Cursor", "Selection"];
const ANSI_LABELS: [&str; 8] = [
    "Black", "Red", "Green", "Yellow", "Blue", "Magenta", "Cyan", "White",
];

/// A scheme's colors with one key replaced.
pub fn with_color(colors: &CustomSchemeColors, key: &str, value: &str) -> CustomSchemeColors {
    let mut record = serde_json::to_value(colors).unwrap_or_default();
    if let Some(object) = record.as_object_mut() {
        object.insert(
            key.to_string(),
            serde_json::Value::String(value.to_string()),
        );
    }
    serde_json::from_value(record).unwrap_or_else(|_| colors.clone())
}

fn css(value: &str) -> Hsla {
    try_hex(value).unwrap_or_else(|| hex(MISSING_SCHEME_COLOR))
}

impl SettingsWindow {
    fn update_custom_scheme(
        &self,
        id: &str,
        cx: &mut muxus_ui::gpui_kit::App,
        change: impl FnOnce(&mut CustomTerminalScheme),
    ) {
        let id = id.to_string();
        self.set_prefs(cx, move |prefs| {
            let mut schemes = term_custom_schemes(prefs);
            if let Some(scheme) = schemes.iter_mut().find(|scheme| scheme.id == id) {
                change(scheme);
            }
            prefs.custom_terminal_schemes = store_custom_schemes(&schemes);
        });
    }

    pub fn custom_schemes_group(
        &mut self,
        theme: &Theme,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let prefs = self.prefs(cx).clone();
        let schemes = term_custom_schemes(&prefs);
        let selected = schemes
            .iter()
            .find(|scheme| scheme.id == self.state.scheme_selected)
            .or_else(|| schemes.first())
            .cloned();
        let at_limit = schemes.len() >= MAX_CUSTOM_TERMINAL_SCHEMES;
        let new_anchor = self.anchor("new-custom-scheme");
        let mut section = group(texts::counted_title("Custom color schemes", schemes.len()))
            .description(secondary_text(
                theme,
                "Your own schemes are offered in the terminal theme pickers above and in every host's terminal appearance. Start from a copy of any scheme, or import a Windows Terminal (.json) or iTerm2 (.itermcolors) color scheme.",
            ))
            .action(
                div()
                    .relative()
                    .child(
                        Button::new("custom-scheme-new", "New")
                            .start_icon("Add")
                            .disabled(at_limit)
                            .on_click(cx.listener(|this, _, _, cx| this.open_new_scheme_menu(cx))),
                    )
                    .child(bounds_probe(&new_anchor)),
            )
            .action(
                Button::new("custom-scheme-import", "Import")
                    .start_icon("UploadFileOutlined")
                    .disabled(at_limit)
                    .on_click(cx.listener(|this, _, _, cx| this.import_custom_schemes(cx))),
            );
        let Some(scheme) = selected else {
            return section
                .row(
                    div().px(px(16.)).py(px(20.)).child(
                        paragraph(
                            "No custom schemes yet. Create one from a copy of any scheme, or import a color scheme file.",
                        )
                        .size(BODY2)
                        .line_height(snap(BODY2 * 1.43))
                        .color(theme.palette.text.secondary)
                        .center(),
                    ),
                )
                .render(theme);
        };
        // The scheme picker, each option a preview.
        let anchor = self.anchor("custom-scheme");
        let open = self.menu_open("custom-scheme");
        let entries: Vec<MenuEntry> = schemes
            .iter()
            .map(|scheme| {
                let preview = terminal_scheme_from_custom(scheme);
                MenuEntry::Item {
                    value: scheme.id.clone(),
                    label: scheme.name.clone().into(),
                    secondary: None,
                    rich: Some(Rc::new(move |theme: &Theme| {
                        scheme_label(theme, &preview, false).into_any_element()
                    })),
                }
            })
            .collect();
        let selected_id = scheme.id.clone();
        let weak = cx.entity().downgrade();
        let picker = super::kit::row_select(
            "select-custom-scheme",
            280.,
            scheme_label(theme, &terminal_scheme_from_custom(&scheme), false).into_any_element(),
            open,
            false,
            &anchor,
            theme,
            Rc::new(move |_, cx| {
                let (entries, selected) = (entries.clone(), selected_id.clone());
                let _ = weak.update(cx, |this, cx| {
                    this.open_menu(
                        "custom-scheme",
                        280.,
                        None,
                        entries,
                        Some(selected),
                        Rc::new(|this, id, _, cx| {
                            this.state.scheme_selected = id;
                            cx.notify();
                        }),
                        cx,
                    );
                });
            }),
        );
        // Only a non-empty name reaches preferences; leaving the field restores the saved one.
        let name_key = format!("custom-scheme-name-{}", scheme.id);
        let (id, saved_name) = (scheme.id.clone(), scheme.name.clone());
        let name_input = self.input(
            &name_key,
            || saved_name.clone(),
            None,
            false,
            window,
            cx,
            move |this, value, _, cx| {
                if !muxus_core::js::js_trim(&value).is_empty() {
                    let value: String = value
                        .chars()
                        .take(MAX_TERMINAL_SCHEME_NAME_LENGTH)
                        .collect();
                    this.update_custom_scheme(&id, cx, move |scheme| scheme.name = value);
                }
            },
        );
        let (blur_key, blur_name) = (name_key.clone(), scheme.name.clone());
        self.on_input_blur(&name_key, move |this, value, window, cx| {
            if muxus_core::js::js_trim(&value).is_empty() {
                this.set_input_value(&blur_key, &blur_name, window, cx);
            }
        });
        self.sync_input(&name_key, &scheme.name, window, cx);
        let name_value = self.input_value(&name_key, cx).unwrap_or_default();
        let unsaved = muxus_core::js::js_trim(&name_value).is_empty();
        let editor = self.scheme_color_editor(theme, &scheme, &prefs.font_family, cx);
        let export_one = scheme.clone();
        let mut actions = div()
            .flex()
            .flex_wrap()
            .gap(px(8.))
            .px(px(16.))
            .py(px(10.))
            .child(
                Button::new("custom-scheme-export", "Export scheme")
                    .outlined()
                    .start_icon("DownloadOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.export_custom_schemes(
                            vec![export_one.clone()],
                            &format!("{} color scheme", export_one.name),
                            format!("Exported {}.", export_one.name),
                            cx,
                        )
                    })),
            );
        if schemes.len() > 1 {
            let all = schemes.clone();
            actions = actions.child(
                Button::new("custom-scheme-export-all", "Export all")
                    .start_icon("DownloadOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.export_custom_schemes(
                            all.clone(),
                            "color schemes",
                            format!("Exported {} color schemes.", all.len()),
                            cx,
                        )
                    })),
            );
        }
        let delete = scheme.clone();
        actions = actions.child(
            div().ml_auto().child(
                Button::new("custom-scheme-delete", "Delete scheme")
                    .color(ButtonColor::Error)
                    .start_icon("DeleteOutlined")
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_custom_scheme(delete.clone(), cx)
                    })),
            ),
        );
        section = section
            .row(
                row(theme, "Scheme")
                    .description_text(theme, "The scheme to edit below.")
                    .control(picker)
                    .render(),
            )
            .row(
                row(theme, "Name")
                    .control(
                        field(SharedString::from(format!("{name_key}-field")), &name_input)
                            .width(280.)
                            .error(unsaved)
                            .render(theme, window, cx),
                    )
                    .render(),
            )
            .row(div().p(px(16.)).child(editor))
            .row(actions);
        section.render(theme)
    }

    fn open_new_scheme_menu(&mut self, cx: &mut Context<Self>) {
        let custom = custom_terminal_schemes(self.prefs(cx));
        let entries = scheme_menu_entries(&custom, "Copy of \u{b7} ");
        self.open_menu(
            "new-custom-scheme",
            260.,
            Some(390.),
            entries,
            None,
            Rc::new(|this, id, _, cx| this.add_scheme_copy(&id, cx)),
            cx,
        );
    }

    fn add_scheme_copy(&mut self, base_id: &str, cx: &mut Context<Self>) {
        let prefs = self.prefs(cx).clone();
        let custom = custom_terminal_schemes(&prefs);
        let base = terminal_scheme(Some(base_id), &custom).clone();
        let current = term_custom_schemes(&prefs);
        match custom_scheme_copy_of(&base, &current) {
            Ok(scheme) => {
                self.state.scheme_selected = scheme.id.clone();
                let mut schemes = current;
                schemes.push(scheme);
                let stored = store_custom_schemes(&schemes);
                self.set_prefs(cx, move |p| p.custom_terminal_schemes = stored);
            }
            Err(error) => self.show_toast(Status::Error, error.to_string(), cx),
        }
    }

    fn delete_custom_scheme(&mut self, scheme: CustomTerminalScheme, cx: &mut Context<Self>) {
        self.confirm(
            format!("Delete {}?", scheme.name),
            "A terminal theme set to this scheme returns to its default, and hosts assigned to it follow the application setting again.",
            "Delete scheme",
            true,
            move |this, _, cx| {
                let id = scheme.id.clone();
                this.drop_inputs(&format!("custom-scheme-name-{id}"));
                this.set_prefs(cx, move |p| p.without_custom_terminal_scheme(&id));
                this.state.scheme_selected.clear();
            },
            cx,
        );
    }

    fn export_custom_schemes(
        &mut self,
        schemes: Vec<CustomTerminalScheme>,
        title: &str,
        message: String,
        cx: &mut Context<Self>,
    ) {
        match create_terminal_scheme_document(&schemes) {
            Ok(document) => {
                let text = serde_json::to_string_pretty(&document).unwrap_or_default() + "\n";
                self.save_text_file_then(
                    export_filename(title, "muxus-scheme.json"),
                    text,
                    cx,
                    move |this, cx| this.show_toast(Status::Success, message, cx),
                );
            }
            Err(error) => self.show_toast(Status::Error, error.to_string(), cx),
        }
    }

    fn import_custom_schemes(&mut self, cx: &mut Context<Self>) {
        self.pick_file(cx, |this, path, cx| {
            let size = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            if size as usize > MAX_TERMINAL_SCHEME_FILE_BYTES {
                this.show_toast(Status::Error, "That color scheme file is too large.", cx);
                return;
            }
            let text = match std::fs::read_to_string(&path) {
                Ok(text) => text,
                Err(error) => {
                    this.show_toast(Status::Error, error.to_string(), cx);
                    return;
                }
            };
            let filename = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            let current = term_custom_schemes(this.prefs(cx));
            let result = parse_terminal_scheme_file(&text, &filename).and_then(|imported| {
                merge_custom_terminal_schemes(&current, &imported).map(|merged| (imported, merged))
            });
            match result {
                Ok((imported, merged)) => {
                    let first = imported
                        .first()
                        .and_then(|first| merged.iter().find(|scheme| scheme.id == first.id))
                        .cloned();
                    this.state.scheme_selected =
                        first.as_ref().map(|s| s.id.clone()).unwrap_or_default();
                    for scheme in &imported {
                        this.drop_inputs(&format!("custom-scheme-name-{}", scheme.id));
                    }
                    let stored = store_custom_schemes(&merged);
                    this.set_prefs(cx, move |p| p.custom_terminal_schemes = stored);
                    let message = match (imported.len(), first) {
                        (1, Some(first)) => format!("Imported {}.", first.name),
                        (count, _) => format!("Imported {count} color schemes."),
                    };
                    this.show_toast(Status::Success, message, cx);
                }
                Err(error) => this.show_toast(Status::Error, error.to_string(), cx),
            }
        });
    }

    /// `SchemeColorEditor`: the preview, the base colors and the ANSI grid.
    fn scheme_color_editor(
        &mut self,
        theme: &Theme,
        scheme: &CustomTerminalScheme,
        font_family: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let p = theme.palette;
        let mut base = div().flex().flex_wrap().gap_x(px(24.)).gap_y(px(8.));
        for (key, title) in SCHEME_BASE_COLOR_KEYS.iter().zip(BASE_LABELS) {
            base = base.child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(self.scheme_well(theme, scheme, key, cx))
                    .child(
                        label(title)
                            .size(BODY2)
                            .line_height(snap(BODY2 * 1.43))
                            .color(p.text.primary),
                    ),
            );
        }
        let cell = || div().flex_1().flex().justify_center();
        let mut header = div()
            .flex()
            .items_center()
            .gap(px(8.))
            .child(div().w(px(56.)));
        for title in ANSI_LABELS {
            header = header.child(
                cell().child(
                    label(title)
                        .size(CAPTION)
                        .line_height(snap(CAPTION * 1.66))
                        .color(p.text.secondary),
                ),
            );
        }
        let mut grid = div().flex().flex_col().gap(px(6.)).child(header);
        for (title, keys) in [
            ("Normal", SCHEME_ANSI_COLOR_KEYS),
            ("Bright", SCHEME_BRIGHT_ANSI_COLOR_KEYS),
        ] {
            let mut line = div().flex().items_center().gap(px(8.)).child(
                div().w(px(56.)).child(
                    label(title)
                        .size(BODY2)
                        .line_height(snap(BODY2 * 1.43))
                        .color(p.text.primary),
                ),
            );
            for key in keys {
                line = line.child(cell().child(self.scheme_well(theme, scheme, key, cx)));
            }
            grid = grid.child(line);
        }
        div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(scheme_preview(theme, &scheme.colors, font_family))
            .child(base)
            .child(grid)
            .into_any_element()
    }

    fn scheme_well(
        &mut self,
        theme: &Theme,
        scheme: &CustomTerminalScheme,
        key: &'static str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let value = scheme
            .colors
            .get(key)
            .unwrap_or(MISSING_SCHEME_COLOR)
            .to_string();
        let anchor_key = format!("scheme-{}-{key}", scheme.id);
        let anchor = self.anchor(&anchor_key);
        let (id, weak) = (scheme.id.clone(), cx.entity().downgrade());
        color_well(
            SharedString::from(format!("{anchor_key}-well")),
            theme,
            css(&value),
            34.,
            28.,
            Rc::new(move |_, cx| {
                let (id, anchor_key, value) = (id.clone(), anchor_key.clone(), value.clone());
                let _ = weak.update(cx, |this, cx| {
                    this.open_color_picker(
                        &anchor_key,
                        &value,
                        Rc::new(move |this, value, cx| {
                            this.update_custom_scheme(&id, cx, |scheme| {
                                scheme.colors = with_color(&scheme.colors, key, &value);
                            });
                        }),
                        cx,
                    );
                });
            }),
        )
        .child(bounds_probe(&anchor))
        .into_any_element()
    }
}

/// A few lines of typical terminal output painted with the scheme being edited.
fn scheme_preview(theme: &Theme, colors: &CustomSchemeColors, font_family: &str) -> AnyElement {
    let family: SharedString = muxus_term::font_catalog::terminal_font_stack(font_family)
        .first()
        .cloned()
        .unwrap_or_else(|| muxus_ui::theme::fonts::MONO.to_string())
        .trim_matches('"')
        .to_string()
        .into();
    let color = |key: &str| css(colors.get(key).unwrap_or(MISSING_SCHEME_COLOR));
    let span = |key: &str, text: &str| div().text_color(color(key)).child(text.to_string());
    let plain = |text: &str| div().child(text.to_string());
    let line = || div().flex().h(px(snap(12.5 * 1.6)));
    let prompt = |line: muxus_ui::gpui_kit::Div| {
        line.child(span("green", "deploy@web-01"))
            .child(plain(":"))
            .child(span("blue", "~/app"))
            .child(plain("$ "))
    };
    let list = [
        ("blue", "conf"),
        ("cyan", "current"),
        ("green", "deploy.sh"),
        ("magenta", "logo.png"),
        ("red", "backup.tar"),
        ("yellow", "notes.txt"),
    ];
    let mut second = line();
    for (key, text) in list {
        second = second.child(span(key, text)).child(plain("  "));
    }
    second = second.child(plain("README.md"));
    let levels = [
        ("brightRed", "ERROR"),
        ("brightYellow", "WARN"),
        ("brightBlue", "INFO"),
        ("brightMagenta", "DEBUG"),
        ("brightCyan", "TRACE"),
        ("brightGreen", "ok"),
    ];
    let mut fourth = line();
    for (key, text) in levels {
        fourth = fourth.child(span(key, text)).child(plain("  "));
    }
    fourth = fourth.child(span("brightBlack", "# bright colors"));
    div()
        .flex()
        .flex_col()
        .px(px(12.))
        .py(px(10.))
        .border_1()
        .border_color(theme.palette.divider)
        .rounded(px(8.))
        .bg(color("background"))
        .text_color(color("foreground"))
        .font_family(family)
        .text_size(px(12.5))
        .line_height(px(snap(12.5 * 1.6)))
        .whitespace_nowrap()
        .overflow_hidden()
        .child(prompt(line()).child(plain("ls")))
        .child(second)
        .child(prompt(line()).child(plain("tail app.log")))
        .child(fourth)
        .child(
            prompt(line())
                .child(
                    div()
                        .bg(color("selectionBackground"))
                        .child("docker compose up"),
                )
                .child(div().bg(color("cursor")).child(" ")),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replaces_one_color_by_its_file_key() {
        let scheme = custom_scheme_copy_of(
            muxus_term::palette::terminal_scheme(Some("vscode-dark"), &[]),
            &[],
        )
        .unwrap();
        let colors = with_color(&scheme.colors, "brightCyan", "#123456");
        assert_eq!(colors.get("brightCyan"), Some("#123456"));
        assert_eq!(colors.get("background"), scheme.colors.get("background"));
    }
}
