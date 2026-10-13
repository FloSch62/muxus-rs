//! Built-in terminal color schemes and the per-host overrides layered on top of them
//! (port of `client/src/terminal/palette.ts`).

use std::borrow::Cow;
use std::sync::LazyLock;

use crate::color::{Rgba, parse_css_color, xterm_256_color};

/// WCAG AA contrast floor enforced for every rendered text cell.
pub const TERMINAL_MINIMUM_CONTRAST_RATIO: f64 = 4.5;

/// Transparent overview-ruler border every scheme carries: marks float over the terminal with no
/// visible ruler chrome.
pub const OVERVIEW_RULER_BORDER_NONE: &str = "#00000000";

/// A terminal palette in CSS notation, the shape of xterm's `ITheme`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalTheme {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub cursor_accent: String,
    pub selection_background: String,
    /// ANSI colors 0–15: black, red, green, yellow, blue, magenta, cyan, white, then the bright
    /// variants in the same order.
    pub ansi: [String; 16],
    pub overview_ruler_border: Option<String>,
}

/// Names of the 16 ANSI slots, as xterm's `ITheme` keys them.
pub const ANSI_COLOR_KEYS: [&str; 16] = [
    "black",
    "red",
    "green",
    "yellow",
    "blue",
    "magenta",
    "cyan",
    "white",
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];

impl TerminalTheme {
    /// A color by its xterm `ITheme` key (`background`, `brightWhite`, …).
    pub fn color(&self, key: &str) -> Option<&str> {
        match key {
            "background" => Some(&self.background),
            "foreground" => Some(&self.foreground),
            "cursor" => Some(&self.cursor),
            "cursorAccent" => Some(&self.cursor_accent),
            "selectionBackground" => Some(&self.selection_background),
            "overviewRulerBorder" => self.overview_ruler_border.as_deref(),
            _ => ANSI_COLOR_KEYS
                .iter()
                .position(|k| *k == key)
                .map(|i| self.ansi[i].as_str()),
        }
    }
}

/// A selectable scheme: a built-in one or one of the user's custom schemes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalScheme {
    pub id: String,
    pub name: String,
    /// Reads as a light background (selection and contrast handling differ).
    pub light: bool,
    pub theme: TerminalTheme,
}

#[allow(clippy::too_many_arguments)]
fn theme(
    background: &str,
    foreground: &str,
    cursor: &str,
    cursor_accent: &str,
    selection_background: &str,
    ansi: [&str; 16],
) -> TerminalTheme {
    TerminalTheme {
        background: background.into(),
        foreground: foreground.into(),
        cursor: cursor.into(),
        cursor_accent: cursor_accent.into(),
        selection_background: selection_background.into(),
        ansi: ansi.map(String::from),
        overview_ruler_border: None,
    }
}

/// The scheme with xterm's overview-ruler border removed. xterm outlines the ruler with
/// `overviewRulerBorder`, which defaults to the foreground color — a bright line down the right
/// edge.
pub fn without_overview_ruler_border(mut theme: TerminalTheme) -> TerminalTheme {
    theme.overview_ruler_border = Some(OVERVIEW_RULER_BORDER_NONE.into());
    theme
}

fn builtin_schemes() -> Vec<TerminalScheme> {
    // VS Code Dark Modern chrome with the default ANSI palette from its terminal color registry.
    let vscode_dark = theme(
        "#181818",
        "#cccccc",
        "#cccccc",
        "#181818",
        "#264f78",
        [
            "#000000", "#cd3131", "#0dbc79", "#e5e510", "#2472c8", "#bc3fbc", "#11a8cd", "#e5e5e5",
            "#666666", "#f14c4c", "#23d18b", "#f5f543", "#3b8eea", "#d670d6", "#29b8db", "#e5e5e5",
        ],
    );
    // Fixed near-black tuned to sit next to the Muxus dark chrome, with a periwinkle-forward ANSI
    // ramp to match the app accent.
    let muxus = theme(
        "#16161e",
        "#c8ccd8",
        "#c8ccd8",
        "#16161e",
        "rgba(110, 139, 251, 0.32)",
        [
            "#1a1b26", "#f7768e", "#9ece6a", "#e0af68", "#7aa2f7", "#bb9af7", "#7dcfff", "#a9b1d6",
            "#414868", "#ff899d", "#b9f27c", "#ffbf7a", "#8db0ff", "#c7a9ff", "#a4daff", "#c0caf5",
        ],
    );
    let dracula = theme(
        "#282a36",
        "#f8f8f2",
        "#f8f8f2",
        "#282a36",
        "rgba(68, 71, 90, 0.7)",
        [
            "#21222c", "#ff5555", "#50fa7b", "#f1fa8c", "#bd93f9", "#ff79c6", "#8be9fd", "#f8f8f2",
            "#6272a4", "#ff6e6e", "#69ff94", "#ffffa5", "#d6acff", "#ff92df", "#a4ffff", "#ffffff",
        ],
    );
    let one_dark = theme(
        "#282c34",
        "#abb2bf",
        "#528bff",
        "#282c34",
        "rgba(62, 68, 81, 0.8)",
        [
            "#282c34", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#abb2bf",
            "#5c6370", "#e06c75", "#98c379", "#e5c07b", "#61afef", "#c678dd", "#56b6c2", "#ffffff",
        ],
    );
    let nord = theme(
        "#2e3440",
        "#d8dee9",
        "#d8dee9",
        "#2e3440",
        "rgba(76, 86, 106, 0.6)",
        [
            "#3b4252", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#88c0d0", "#e5e9f0",
            "#4c566a", "#bf616a", "#a3be8c", "#ebcb8b", "#81a1c1", "#b48ead", "#8fbcbb", "#eceff4",
        ],
    );
    let gruvbox_dark = theme(
        "#282828",
        "#ebdbb2",
        "#ebdbb2",
        "#282828",
        "rgba(80, 73, 69, 0.7)",
        [
            "#282828", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#a89984",
            "#928374", "#fb4934", "#b8bb26", "#fabd2f", "#83a598", "#d3869b", "#8ec07c", "#ebdbb2",
        ],
    );
    let catppuccin_mocha = theme(
        "#1e1e2e",
        "#cdd6f4",
        "#f5e0dc",
        "#1e1e2e",
        "rgba(88, 91, 112, 0.6)",
        [
            "#45475a", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#bac2de",
            "#585b70", "#f38ba8", "#a6e3a1", "#f9e2af", "#89b4fa", "#f5c2e7", "#94e2d5", "#a6adc8",
        ],
    );
    let monokai = theme(
        "#272822",
        "#f8f8f2",
        "#f8f8f2",
        "#272822",
        "rgba(73, 72, 62, 0.8)",
        [
            "#272822", "#f92672", "#a6e22e", "#e6db74", "#66d9ef", "#ae81ff", "#a1efe4", "#f8f8f2",
            "#75715e", "#f92672", "#a6e22e", "#e6db74", "#66d9ef", "#ae81ff", "#a1efe4", "#f9f8f5",
        ],
    );
    let solarized_dark = theme(
        "#002b36",
        "#839496",
        "#839496",
        "#002b36",
        "rgba(7, 54, 66, 0.9)",
        [
            "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
            "#586e75", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3",
        ],
    );
    let solarized_light = theme(
        "#fdf6e3",
        "#657b83",
        "#657b83",
        "#fdf6e3",
        "rgba(238, 232, 213, 1)",
        [
            "#073642", "#dc322f", "#859900", "#b58900", "#268bd2", "#d33682", "#2aa198", "#eee8d5",
            "#93a1a1", "#cb4b16", "#586e75", "#657b83", "#839496", "#6c71c4", "#93a1a1", "#fdf6e3",
        ],
    );
    // Warm, low-glare paper with deliberately restrained ANSI colors.
    let paper = theme(
        "#f5f0e6",
        "#403b33",
        "#765b3d",
        "#f5f0e6",
        "rgba(183, 157, 111, 0.38)",
        [
            "#282622", "#a63d40", "#52783d", "#866316", "#386b8c", "#795080", "#39766f", "#d8d1c3",
            "#706a60", "#bd4f4b", "#648947", "#9d741b", "#4b7b9a", "#8d6092", "#4a8780", "#fffaf0",
        ],
    );
    // VS Code's light terminal palette, with its pale-blue selection. ANSI bright white stays
    // genuinely light: tools such as GNU ls use bold white over a red background to call out
    // broken symlinks.
    let vscode_light = theme(
        "#ffffff",
        "#333333",
        "#333333",
        "#ffffff",
        "#add6ff",
        [
            "#000000", "#cd3131", "#008000", "#795e26", "#0451a5", "#bc05bc", "#0598bc", "#767676",
            "#666666", "#cd3131", "#16825d", "#8f6d20", "#0451a5", "#bc05bc", "#0598bc", "#ffffff",
        ],
    );
    let github_light = theme(
        "#ffffff",
        "#24292f",
        "#0969da",
        "#ffffff",
        "rgba(9, 105, 218, 0.24)",
        [
            "#24292f", "#cf222e", "#1a7f37", "#9a6700", "#0969da", "#8250df", "#1b7c83", "#d0d7de",
            "#57606a", "#a40e26", "#116329", "#7d4e00", "#0550ae", "#6639ba", "#0a676d", "#ffffff",
        ],
    );
    let gruvbox_light = theme(
        "#fbf1c7",
        "#3c3836",
        "#3c3836",
        "#fbf1c7",
        "#d5c4a1",
        [
            "#282828", "#9d0006", "#79740e", "#b57614", "#076678", "#8f3f71", "#427b58", "#d5c4a1",
            "#7c6f64", "#cc241d", "#98971a", "#d79921", "#458588", "#b16286", "#689d6a", "#fbf1c7",
        ],
    );
    let catppuccin_latte = theme(
        "#eff1f5",
        "#4c4f69",
        "#dc8a78",
        "#eff1f5",
        "rgba(172, 176, 190, 0.55)",
        [
            "#5c5f77", "#d20f39", "#40a02b", "#b36b00", "#1e66f5", "#8839ef", "#0f7f85", "#acb0be",
            "#6c6f85", "#b80b31", "#348822", "#965800", "#1856ce", "#762fd3", "#0b6b70", "#eff1f5",
        ],
    );

    let entries: [(&str, &str, bool, TerminalTheme); 15] = [
        ("paper", "Paper", true, paper),
        ("vscode-light", "Light (VS Code)", true, vscode_light),
        ("github-light", "GitHub Light", true, github_light),
        ("gruvbox-light", "Gruvbox Light", true, gruvbox_light),
        (
            "catppuccin-latte",
            "Catppuccin Latte",
            true,
            catppuccin_latte,
        ),
        ("solarized-light", "Solarized Light", true, solarized_light),
        ("vscode-dark", "Dark (VS Code)", false, vscode_dark),
        ("muxus", "Muxus", false, muxus),
        ("dracula", "Dracula", false, dracula),
        ("one-dark", "One Dark", false, one_dark),
        ("nord", "Nord", false, nord),
        ("gruvbox-dark", "Gruvbox Dark", false, gruvbox_dark),
        (
            "catppuccin-mocha",
            "Catppuccin Mocha",
            false,
            catppuccin_mocha,
        ),
        ("monokai", "Monokai", false, monokai),
        ("solarized-dark", "Solarized Dark", false, solarized_dark),
    ];
    entries
        .into_iter()
        .map(|(id, name, light, theme)| TerminalScheme {
            id: id.into(),
            name: name.into(),
            light,
            theme: without_overview_ruler_border(theme),
        })
        .collect()
}

static TERMINAL_SCHEMES: LazyLock<Vec<TerminalScheme>> = LazyLock::new(builtin_schemes);

/// Built-in terminal color schemes. The default matches VS Code's stock dark terminal so the
/// out-of-the-box look is what most people already stare at all day; the rest are faithful
/// renditions of the classics people expect from a terminal, plus a few light palettes tuned to
/// stay readable for a full workday.
pub fn terminal_schemes() -> &'static [TerminalScheme] {
    &TERMINAL_SCHEMES
}

/// Id of the scheme every unknown or missing id falls back to.
pub const DEFAULT_SCHEME_ID: &str = "vscode-dark";

fn builtin_scheme(id: &str) -> Option<&'static TerminalScheme> {
    TERMINAL_SCHEMES.iter().find(|scheme| scheme.id == id)
}

/// Whether an id names a built-in scheme or one of the user's custom schemes.
pub fn is_terminal_scheme_id(value: &str, custom_schemes: &[TerminalScheme]) -> bool {
    builtin_scheme(value).is_some() || custom_schemes.iter().any(|scheme| scheme.id == value)
}

/// A stale or future host override must inherit the current app preference.
pub fn terminal_scheme_id_for_host<'a>(
    application_scheme_id: &'a str,
    host_scheme_id: Option<&'a str>,
    custom_schemes: &[TerminalScheme],
) -> &'a str {
    match host_scheme_id {
        Some(id) if is_terminal_scheme_id(id, custom_schemes) => id,
        _ => application_scheme_id,
    }
}

/// `#rrggbb` exactly, as color wells store it.
pub fn is_hex_color(value: &str) -> bool {
    value.len() == 7 && value.starts_with('#') && value[1..].chars().all(|c| c.is_ascii_hexdigit())
}

/// A valid host color wins; malformed or absent metadata inherits the app value.
pub fn terminal_color_for_host<'a>(
    application_color: &'a str,
    host_color: Option<&'a str>,
) -> &'a str {
    match host_color {
        Some(color) if !color.is_empty() && is_hex_color(color) => color,
        _ => application_color,
    }
}

/// Resolve a scheme id among the built-in schemes and the user's custom ones, falling back to the
/// VS Code dark default.
pub fn terminal_scheme<'a>(
    id: Option<&str>,
    custom_schemes: &'a [TerminalScheme],
) -> &'a TerminalScheme {
    let default = || builtin_scheme(DEFAULT_SCHEME_ID).expect("default scheme exists");
    let Some(id) = id else { return default() };
    builtin_scheme(id)
        .or_else(|| custom_schemes.iter().find(|scheme| scheme.id == id))
        .unwrap_or_else(default)
}

/// The scheme's theme with the user's font color replacing the default foreground. Only unstyled
/// text changes; ANSI-colored output keeps the scheme palette. An empty or malformed override
/// leaves the theme alone, so a stale stored value cannot blank the terminal.
pub fn theme_with_font_color<'a>(
    theme: &'a TerminalTheme,
    font_color: &str,
) -> Cow<'a, TerminalTheme> {
    theme_with_color_overrides(theme, font_color, "")
}

/// Apply valid user-selected foreground and background colors to a scheme.
pub fn theme_with_color_overrides<'a>(
    theme: &'a TerminalTheme,
    font_color: &str,
    background_color: &str,
) -> Cow<'a, TerminalTheme> {
    let foreground = is_hex_color(font_color).then_some(font_color);
    let background = is_hex_color(background_color).then_some(background_color);
    if foreground.is_none() && background.is_none() {
        return Cow::Borrowed(theme);
    }
    let mut next = theme.clone();
    if let Some(color) = foreground {
        next.foreground = color.into();
    }
    if let Some(color) = background {
        next.background = color.into();
    }
    Cow::Owned(next)
}

/// A theme parsed for painting: every color opaque except the selection, plus the full xterm
/// 256-color table.
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedPalette {
    pub background: Rgba,
    pub foreground: Rgba,
    pub cursor: Rgba,
    pub cursor_accent: Rgba,
    /// The selection as the theme gives it (may be translucent).
    pub selection_background: Rgba,
    /// The selection flattened over the background, as xterm's `selectionBackgroundOpaque`.
    pub selection_background_opaque: Rgba,
    /// Unfocused terminals select with the same color in Muxus (xterm's inactive default
    /// follows the active one when the theme sets no inactive color).
    pub selection_inactive_background_opaque: Rgba,
    pub ansi: [Rgba; 256],
}

impl ResolvedPalette {
    pub fn new(theme: &TerminalTheme) -> Self {
        let parse = |value: &str, fallback: Rgba| parse_css_color(value).unwrap_or(fallback);
        let background = parse(&theme.background, Rgba::rgb(0, 0, 0)).with_alpha(255);
        let foreground = parse(&theme.foreground, Rgba::rgb(0xff, 0xff, 0xff)).with_alpha(255);
        let cursor = parse(&theme.cursor, foreground);
        let cursor_accent = parse(&theme.cursor_accent, background);
        // xterm's default selection is white at 30% when a theme leaves it out.
        let selection_background = parse(
            &theme.selection_background,
            Rgba {
                r: 255,
                g: 255,
                b: 255,
                a: 77,
            },
        );
        let selection_background_opaque = selection_background.over(background);
        let mut ansi = [Rgba::default(); 256];
        for (index, slot) in ansi.iter_mut().enumerate() {
            *slot = if index < 16 {
                parse(&theme.ansi[index], Rgba::rgb(0, 0, 0)).with_alpha(255)
            } else {
                xterm_256_color(index as u8)
            };
        }
        Self {
            background,
            foreground,
            cursor,
            cursor_accent,
            selection_background,
            selection_background_opaque,
            selection_inactive_background_opaque: selection_background_opaque,
            ansi,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const NEW_LIGHT_SCHEME_IDS: [&str; 5] = [
        "paper",
        "vscode-light",
        "github-light",
        "gruvbox-light",
        "catppuccin-latte",
    ];

    fn relative_luminance(color: &str) -> f64 {
        let channel = |offset: usize| {
            let value =
                f64::from(u8::from_str_radix(&color[offset..offset + 2], 16).unwrap()) / 255.0;
            if value <= 0.04045 {
                value / 12.92
            } else {
                ((value + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5)
    }

    fn contrast_ratio(first: &str, second: &str) -> f64 {
        let (a, b) = (relative_luminance(first), relative_luminance(second));
        let (lighter, darker) = if a > b { (a, b) } else { (b, a) };
        (lighter + 0.05) / (darker + 0.05)
    }

    #[test]
    fn offers_the_new_light_schemes_with_unique_ids() {
        let ids: Vec<&str> = terminal_schemes().iter().map(|s| s.id.as_str()).collect();
        let unique: std::collections::HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), ids.len());
        for id in NEW_LIGHT_SCHEME_IDS {
            assert!(ids.contains(&id));
            assert!(terminal_scheme(Some(id), &[]).light);
        }
    }

    #[test]
    fn removes_the_overview_ruler_border_from_every_built_in_scheme() {
        for scheme in terminal_schemes() {
            assert_eq!(
                scheme.theme.overview_ruler_border.as_deref(),
                Some("#00000000")
            );
        }
    }

    #[test]
    fn keeps_bold_white_readable_on_red_status_backgrounds_in_light_schemes() {
        for scheme in terminal_schemes().iter().filter(|s| s.light) {
            let ratio = contrast_ratio(&scheme.theme.ansi[15], &scheme.theme.ansi[1]);
            assert!(ratio > 4.0, "{}", scheme.name);
        }
    }

    #[test]
    fn defines_a_complete_ansi_palette_for_every_offered_scheme() {
        for scheme in terminal_schemes() {
            assert!(
                is_hex_color(&scheme.theme.background),
                "{} background",
                scheme.name
            );
            assert!(
                is_hex_color(&scheme.theme.foreground),
                "{} foreground",
                scheme.name
            );
            for (index, color) in scheme.theme.ansi.iter().enumerate() {
                assert!(
                    is_hex_color(color),
                    "{} {}",
                    scheme.name,
                    ANSI_COLOR_KEYS[index]
                );
            }
        }
    }

    #[test]
    fn enforces_wcag_aa_text_contrast_while_rendering_every_scheme() {
        const { assert!(TERMINAL_MINIMUM_CONTRAST_RATIO >= 4.5) };
    }

    #[test]
    fn keeps_vscode_dark_as_the_fallback_even_though_light_schemes_are_listed_first() {
        assert_eq!(terminal_scheme(None, &[]).id, "vscode-dark");
        assert_eq!(terminal_scheme(Some("unknown"), &[]).id, "vscode-dark");
    }

    #[test]
    fn uses_a_valid_host_override_and_otherwise_inherits_the_application_scheme() {
        assert_eq!(
            terminal_scheme_id_for_host("paper", Some("dracula"), &[]),
            "dracula"
        );
        assert_eq!(terminal_scheme_id_for_host("paper", None, &[]), "paper");
        assert_eq!(
            terminal_scheme_id_for_host("paper", Some("removed-scheme"), &[]),
            "paper"
        );
    }

    #[test]
    fn uses_valid_host_colors_and_inherits_when_host_metadata_is_absent_or_malformed() {
        assert_eq!(
            terminal_color_for_host("#112233", Some("#aabbcc")),
            "#aabbcc"
        );
        assert_eq!(terminal_color_for_host("#112233", None), "#112233");
        assert_eq!(
            terminal_color_for_host("#112233", Some("orange")),
            "#112233"
        );
    }

    #[test]
    fn font_color_replaces_only_the_foreground() {
        let theme = &terminal_scheme(Some("vscode-dark"), &[]).theme;
        let overridden = theme_with_font_color(theme, "#FF8800");
        assert_eq!(overridden.foreground, "#FF8800");
        let mut restored = overridden.into_owned();
        restored.foreground = theme.foreground.clone();
        assert_eq!(&restored, theme);
        assert_eq!(theme.foreground, "#cccccc");
    }

    #[test]
    fn font_color_leaves_the_scheme_alone_for_the_empty_default_and_malformed_values() {
        let theme = &terminal_scheme(Some("vscode-dark"), &[]).theme;
        for value in ["", "orange", "#ff0"] {
            assert!(matches!(
                theme_with_font_color(theme, value),
                Cow::Borrowed(_)
            ));
        }
    }

    #[test]
    fn color_overrides_replace_foreground_and_background_independently() {
        let theme = &terminal_scheme(Some("vscode-dark"), &[]).theme;
        let both = theme_with_color_overrides(theme, "#FF8800", "#102030");
        assert_eq!(
            (both.foreground.as_str(), both.background.as_str()),
            ("#FF8800", "#102030")
        );
        assert_eq!(
            (theme.foreground.as_str(), theme.background.as_str()),
            ("#cccccc", "#181818")
        );

        let background_only = theme_with_color_overrides(theme, "", "#102030");
        assert_eq!(background_only.foreground, theme.foreground);
        assert_eq!(background_only.background, "#102030");
        let foreground_only = theme_with_color_overrides(theme, "#FF8800", "");
        assert_eq!(foreground_only.foreground, "#FF8800");
        assert_eq!(foreground_only.background, theme.background);
    }

    #[test]
    fn color_overrides_leave_the_scheme_alone_when_neither_is_valid() {
        let theme = &terminal_scheme(Some("vscode-dark"), &[]).theme;
        assert!(matches!(
            theme_with_color_overrides(theme, "", ""),
            Cow::Borrowed(_)
        ));
        assert!(matches!(
            theme_with_color_overrides(theme, "orange", "#123"),
            Cow::Borrowed(_)
        ));
    }

    #[test]
    fn resolves_a_theme_for_painting() {
        let palette = ResolvedPalette::new(&terminal_scheme(Some("muxus"), &[]).theme);
        assert_eq!(palette.background, Rgba::rgb(0x16, 0x16, 0x1e));
        assert_eq!(palette.selection_background.a, 82);
        assert_eq!(palette.selection_background_opaque.a, 255);
        assert_eq!(palette.ansi[1], Rgba::rgb(0xf7, 0x76, 0x8e));
        assert_eq!(palette.ansi[231], Rgba::rgb(0xff, 0xff, 0xff));
    }
}
