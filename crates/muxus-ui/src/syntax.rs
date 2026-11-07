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
    /// `editor.wordHighlightTextBackground`: occurrences of the word at the cursor.
    pub word_highlight: Hsla,
    /// `editor.selectionHighlightBackground`: other occurrences of the selection.
    pub selection_highlight: Hsla,
    /// `editor.rangeHighlightBackground`: the line Go to Line previews.
    pub range_highlight: Hsla,
    /// `editorError.foreground`, `editorWarning.foreground`: squiggles.
    pub error: Hsla,
    pub warning: Hsla,
    /// `editorLink.activeForeground`.
    pub active_link: Hsla,
    /// The overview ruler: its border, occurrence marks, error marks, the cursor (the cursor
    /// color at 70%).
    pub overview_ruler_border: Hsla,
    pub overview_ruler_occurrence: Hsla,
    pub overview_ruler_error: Hsla,
    pub overview_ruler_cursor: Hsla,
    /// The quick input (Go to Line): `quickInput.background`, its foreground, the input box,
    /// the focused row (`quickInputList.focusBackground`/`Foreground`), `widget.shadow`.
    pub quick_input_background: Hsla,
    pub quick_input_foreground: Hsla,
    pub quick_input_field: Hsla,
    pub quick_input_border: Hsla,
    pub quick_input_focus: Hsla,
    pub quick_input_focus_ink: Hsla,
    pub widget_shadow: Hsla,
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
                word_highlight: hex("#575757b8"),
                selection_highlight: hex("#add6ff26"),
                range_highlight: hex("#ffffff0b"),
                error: hex("#f14c4c"),
                warning: hex("#cca700"),
                active_link: hex("#4e94ce"),
                overview_ruler_border: hex("#7f7f7f4d"),
                overview_ruler_occurrence: hex("#a0a0a0cc"),
                overview_ruler_error: hex("#ff1212b3"),
                overview_ruler_cursor: hex("#aeafadb3"),
                quick_input_background: hex("#252526"),
                quick_input_foreground: hex("#cccccc"),
                quick_input_field: hex("#3c3c3c"),
                quick_input_border: hex("#007fd4"),
                quick_input_focus: hex("#04395e"),
                quick_input_focus_ink: hex("#ffffff"),
                widget_shadow: hex("#0000005c"),
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
                word_highlight: hex("#57575740"),
                selection_highlight: hex("#add6ff4d"),
                range_highlight: hex("#fdff0033"),
                error: hex("#e51400"),
                warning: hex("#bf8803"),
                active_link: hex("#0000ff"),
                overview_ruler_border: hex("#7f7f7f4d"),
                overview_ruler_occurrence: hex("#a0a0a0cc"),
                overview_ruler_error: hex("#ff1212b3"),
                overview_ruler_cursor: hex("#000000b3"),
                quick_input_background: hex("#f3f3f3"),
                quick_input_foreground: hex("#616161"),
                quick_input_field: hex("#ffffff"),
                quick_input_border: hex("#0090f1"),
                quick_input_focus: hex("#0060c0"),
                quick_input_focus_ink: hex("#ffffff"),
                widget_shadow: hex("#00000029"),
            }
        }
    }
}

/// The remote editor's General Text language, whose highlighter emits Monarch token names
/// (`invalid`, `string.link`, `attribute.name`…) rather than tree-sitter captures.
pub const MONARCH_GENERAL_TEXT: &str = "muxus-text";

/// The token rules of Monaco's `vs` and `vs-dark` themes (`standalone/common/themes.ts`) that
/// apply to every language, as (token, light, dark); `None` keeps the editor's foreground.
const MONARCH_RULES: &[(&str, Option<&str>, Option<&str>)] = &[
    ("invalid", Some("#cd3131"), Some("#f44747")),
    ("variable", Some("#001188"), Some("#74b0df")),
    ("variable.predefined", Some("#4864aa"), Some("#4864aa")),
    ("variable.parameter", Some("#001188"), Some("#9cdcfe")),
    ("constant", Some("#dd0000"), Some("#569cd6")),
    ("comment", Some("#008000"), Some("#608b4e")),
    ("number", Some("#098658"), Some("#b5cea8")),
    ("number.hex", Some("#3030c0"), Some("#5bb498")),
    ("regexp", Some("#800000"), Some("#b46695")),
    ("annotation", Some("#808080"), Some("#cc6666")),
    ("type", Some("#008080"), Some("#3dc9b0")),
    ("delimiter", Some("#000000"), Some("#dcdcdc")),
    ("tag", Some("#800000"), Some("#569cd6")),
    ("metatag", Some("#e00000"), Some("#dd6a6f")),
    ("key", Some("#863b00"), Some("#9cdcfe")),
    ("attribute.name", Some("#ff0000"), Some("#9cdcfe")),
    ("attribute.value", Some("#0451a5"), Some("#ce9178")),
    ("string", Some("#a31515"), Some("#ce9178")),
    ("keyword", Some("#0000ff"), Some("#569cd6")),
    ("keyword.flow", Some("#af00db"), Some("#c586c0")),
];

/// A Monarch token's color: the theme rule for the longest dotted prefix of the token, as
/// Monaco's token theme matches (`string.link` takes `string`).
pub fn monarch_color(dark: bool, token: &str) -> Option<&'static str> {
    let mut prefix = token;
    loop {
        if let Some((_, light, dark_color)) =
            MONARCH_RULES.iter().find(|(name, ..)| *name == prefix)
        {
            return if dark { *dark_color } else { *light };
        }
        prefix = &prefix[..prefix.rfind('.')?];
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
            // A link under Ctrl/Cmd+hover (`editorLink.activeForeground`).
            "link_text" => return pick("#0000ff", "#4e94ce"),
            _ => {}
        }
        if language == MONARCH_GENERAL_TEXT {
            return monarch_color(dark, name);
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

    #[test]
    fn colors_monarch_tokens_by_their_longest_rule() {
        let light = MonacoTokens::new(false, MONARCH_GENERAL_TEXT);
        let dark = MonacoTokens::new(true, MONARCH_GENERAL_TEXT);
        assert_eq!(light.color("string.link"), Some("#a31515"));
        assert_eq!(light.color("number.hex"), Some("#3030c0"));
        assert_eq!(light.color("attribute.name"), Some("#ff0000"));
        assert_eq!(dark.color("regexp"), Some("#b46695"));
        assert_eq!(dark.color("delimiter.curly"), Some("#dcdcdc"));
        assert_eq!(light.color("bracket.depth.2"), Some("#319331"));
        assert_eq!(light.color("unknown"), None);
    }
}
