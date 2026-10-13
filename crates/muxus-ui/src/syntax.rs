//! The code editors' colors: Monaco's `vs` and `vs-dark` themes, as the client's editors
//! (remote files, the management workbench) show them, for gpui-component's tree-sitter
//! grammars. Every code editor in the app takes its style from here.

use std::sync::Arc;

use crate::zoom::px;
use gpui_kit::base::input::{HighlightStyleResolver, InputEditorStyle};
use gpui_kit::{FontStyle, HighlightStyle, Hsla, SharedString, UnderlineStyle};

use crate::color::hex;

/// Monaco's editor colors for one theme.
#[derive(Clone, Copy, Debug)]
pub struct MonacoColors {
    pub background: Hsla,
    pub foreground: Hsla,
    pub line_number: Hsla,
    pub active_line_number: Hsla,
    /// `editor.lineHighlightBorder`: Monaco outlines the cursor's line.
    pub line_highlight_border: Hsla,
    pub selection: Hsla,
    pub cursor: Hsla,
    pub indent_guide: Hsla,
    /// The status bar under an editor (the client's own footer, not Monaco's).
    pub footer_background: Hsla,
    pub footer_ink: Hsla,
}

impl MonacoColors {
    pub fn new(dark: bool) -> Self {
        if dark {
            Self {
                background: hex("#1e1e1e"),
                foreground: hex("#d4d4d4"),
                line_number: hex("#858585"),
                active_line_number: hex("#c6c6c6"),
                line_highlight_border: hex("#282828"),
                selection: hex("#264f78"),
                cursor: hex("#aeafad"),
                indent_guide: hex("#404040"),
                footer_background: hex("#18181b"),
                footer_ink: hex("#b1b1ba"),
            }
        } else {
            Self {
                background: hex("#fffffe"),
                foreground: hex("#000000"),
                line_number: hex("#237893"),
                active_line_number: hex("#0b216f"),
                line_highlight_border: hex("#eeeeee"),
                selection: hex("#add6ff"),
                cursor: hex("#000000"),
                indent_guide: hex("#d3d3d3"),
                footer_background: hex("#f4f4f5"),
                footer_ink: hex("#5f5f68"),
            }
        }
    }
}

/// Tree-sitter capture names resolved to Monaco's token colors for one language (YAML and
/// JSON color keys and values the way Monaco's own tokenizers do).
pub struct MonacoTokens {
    dark: bool,
    language: SharedString,
}

impl MonacoTokens {
    pub fn new(dark: bool, language: impl Into<SharedString>) -> Arc<Self> {
        Arc::new(Self {
            dark,
            language: language.into(),
        })
    }

    /// The CSS color of a capture, `None` for the editor's foreground.
    pub fn color(&self, name: &str) -> Option<&'static str> {
        let dark = self.dark;
        let pick = |light: &'static str, dark_color: &'static str| {
            Some(if dark { dark_color } else { light })
        };
        let language = self.language.as_ref();
        let base = name.split('.').next().unwrap_or(name);
        // Bracket pair colorization: `editorBracketHighlight.foreground1..3` by nesting
        // depth, and the unexpected (unmatched) bracket color.
        match name {
            "bracket.depth.1" => return pick("#0431fa", "#ffd700"),
            "bracket.depth.2" => return pick("#319331", "#da70d6"),
            "bracket.depth.3" => return pick("#7b3814", "#179fff"),
            "bracket.unexpected" => return pick("#ff1212", "#ff1212"),
            _ => {}
        }
        match (language, name) {
            ("yaml", "property") => return pick("#008080", "#3dc9b0"),
            ("yaml", "string") => return pick("#0451a5", "#ce9178"),
            ("json", "property") => return pick("#a31515", "#9cdcfe"),
            ("json", "string") => return pick("#0451a5", "#ce9178"),
            _ => {}
        }
        match base {
            "comment" => pick("#008000", "#6a9955"),
            "keyword" => pick("#0000ff", "#569cd6"),
            "string" => match name {
                "string.regex" | "string.regexp" => pick("#811f3f", "#d16969"),
                "string.escape" | "string.special" => pick("#ff0000", "#d7ba7d"),
                _ => pick("#a31515", "#ce9178"),
            },
            "number" => pick("#098658", "#b5cea8"),
            "boolean" => pick("#0000ff", "#569cd6"),
            "constant" => pick("#0000ff", "#569cd6"),
            "type" => pick("#008080", "#3dc9b0"),
            "tag" => pick("#800000", "#569cd6"),
            "attribute" => pick("#ff0000", "#9cdcfe"),
            "variable" if name != "variable" => pick("#001188", "#74b0df"),
            "label" => pick("#001188", "#74b0df"),
            "link_uri" => pick("#0451a5", "#ce9178"),
            _ => None,
        }
    }
}

impl HighlightStyleResolver for MonacoTokens {
    fn style(&self, name: &str) -> Option<HighlightStyle> {
        let color = self.color(name)?;
        let mut style = HighlightStyle {
            color: Some(hex(color)),
            ..HighlightStyle::default()
        };
        if name == "link_uri" {
            style.underline = Some(UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            });
        }
        if name == "emphasis" {
            style.font_style = Some(FontStyle::Italic);
        }
        Some(style)
    }
}

/// A code editor's style for `language` (a tree-sitter grammar name): Monaco's colors, its
/// token colors, the gutter in the editor's background.
pub fn code_editor_style(dark: bool, language: impl Into<SharedString>) -> InputEditorStyle {
    let colors = MonacoColors::new(dark);
    InputEditorStyle {
        foreground: colors.foreground,
        muted_foreground: colors.line_number,
        background: colors.background,
        // The engine draws indent guides in the border color.
        border: colors.indent_guide,
        selection: colors.selection,
        caret: colors.cursor,
        highlight_styles: MonacoTokens::new(dark, language),
        editor_active_line: None,
        editor_gutter_background: Some(colors.background),
        ..InputEditorStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colors_brackets_by_depth_like_monaco() {
        let light = MonacoTokens::new(false, "json");
        let dark = MonacoTokens::new(true, "json");
        assert_eq!(light.color("bracket.depth.1"), Some("#0431fa"));
        assert_eq!(light.color("bracket.depth.2"), Some("#319331"));
        assert_eq!(light.color("bracket.depth.3"), Some("#7b3814"));
        assert_eq!(dark.color("bracket.depth.1"), Some("#ffd700"));
        assert_eq!(dark.color("bracket.depth.3"), Some("#179fff"));
    }

    #[test]
    fn colors_yaml_like_monaco() {
        let light = MonacoTokens::new(false, "yaml");
        let dark = MonacoTokens::new(true, "yaml");
        assert_eq!(light.color("property"), Some("#008080"));
        assert_eq!(light.color("string"), Some("#0451a5"));
        assert_eq!(dark.color("property"), Some("#3dc9b0"));
        assert_eq!(dark.color("string"), Some("#ce9178"));
        let shell = MonacoTokens::new(false, "bash");
        assert_eq!(shell.color("string"), Some("#a31515"));
        assert_eq!(shell.color("comment"), Some("#008000"));
        assert_eq!(shell.color("punctuation.delimiter"), None);
    }
}
