//! The Muxus theme: a port of `client/src/theme.ts` together with the MUI
//! defaults it builds on (palette augmentation, action colors, typography
//! scale, shape), resolved to concrete values.
//!
//! MUI scales every rem-based size by `fontSize / 14` when the theme sets
//! `typography.fontSize` (13 here), so `pxToRem(20)` is 18.57px, not 20px.
//! [`Typography::px_to_rem`] reproduces that; the variant table below holds
//! the computed sizes, measured in the Electron app's DOM.

use crate::zoom::px;
use gpui_kit::{App, Global, Hsla, Pixels};

use crate::color::{alpha, contrast_ratio, darken, hex, lighten, rgba8};

/// Light or dark, as `themeMode` resolves.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Mode {
    #[default]
    Light,
    Dark,
}

impl Mode {
    pub fn is_dark(self) -> bool {
        self == Mode::Dark
    }
}

/// Shared layout dimensions (`layout` in theme.ts). Several of these are
/// cross-file contracts, so both sides read the same constant.
pub mod layout {
    /// TopBar toolbar height; the window's title bar uses the same value.
    pub const TOP_BAR_HEIGHT: f32 = 52.;
    /// Minimal draggable title bar retained while focus mode hides application chrome.
    pub const FOCUS_TOP_BAR_HEIGHT: f32 = 32.;
    /// Tab strip height under the top bar.
    pub const TAB_STRIP_HEIGHT: f32 = 40.;
    /// Default SFTP panel width.
    pub const SFTP_PANEL_WIDTH: f32 = 380.;
    /// Forwarding side panel width.
    pub const FORWARDING_PANEL_WIDTH: f32 = 380.;
    /// Themed scrollbar thickness.
    pub const SCROLLBAR_SIZE: f32 = 10.;
    /// Status bar height (`STATUS_BAR_HEIGHT`).
    pub const STATUS_BAR_HEIGHT: f32 = 26.;
}

/// One palette color with MUI's derived variants.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PaletteColor {
    pub main: Hsla,
    pub light: Hsla,
    pub dark: Hsla,
    pub contrast_text: Hsla,
}

impl PaletteColor {
    /// MUI `augmentColor`: light = lighten(main, 0.2), dark = darken(main, 0.3),
    /// contrast text white when it reaches a 3:1 ratio, else near-black.
    fn augment(main: Hsla, dark: Option<Hsla>) -> Self {
        let white = hex("#fff");
        Self {
            main,
            light: lighten(main, 0.2),
            dark: dark.unwrap_or_else(|| darken(main, 0.3)),
            contrast_text: if contrast_ratio(main, white) >= 3. {
                white
            } else {
                rgba8(0, 0, 0, 0.87)
            },
        }
    }
}

/// `palette.text`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextColors {
    pub primary: Hsla,
    pub secondary: Hsla,
    /// The dimmest text tier, keyed off the secondary hue (see theme.ts).
    pub disabled: Hsla,
}

/// `palette.action`: MUI's interaction overlays.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActionColors {
    pub active: Hsla,
    pub hover: Hsla,
    pub selected: Hsla,
    pub disabled: Hsla,
    pub disabled_background: Hsla,
    pub focus: Hsla,
    pub hover_opacity: f32,
    pub selected_opacity: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Palette {
    pub mode: Mode,
    pub primary: PaletteColor,
    pub secondary: PaletteColor,
    pub success: PaletteColor,
    pub warning: PaletteColor,
    pub error: PaletteColor,
    pub info: PaletteColor,
    pub divider: Hsla,
    /// Chrome surfaces (top bar, session sidebar, tab strip) share this background.
    pub sidebar: Hsla,
    /// Label color for sidebar rows: in light mode it sits below text.primary,
    /// because a panel of near-black rows reads as a slab of ink.
    pub sidebar_ink: Hsla,
    pub background_default: Hsla,
    pub background_paper: Hsla,
    pub text: TextColors,
    pub action: ActionColors,
    pub common_white: Hsla,
    pub common_black: Hsla,
    /// MUI's grey scale (`palette.grey`), the same in both modes.
    pub grey: Grey,
}

/// `palette.grey`: the MUI grey shades components use for neutral fills and outlines.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Grey {
    pub g50: Hsla,
    pub g100: Hsla,
    pub g200: Hsla,
    pub g300: Hsla,
    pub g400: Hsla,
    pub g500: Hsla,
    pub g600: Hsla,
    pub g700: Hsla,
    pub g800: Hsla,
    pub g900: Hsla,
}

impl Grey {
    fn mui() -> Self {
        Self {
            g50: hex("#fafafa"),
            g100: hex("#f5f5f5"),
            g200: hex("#eeeeee"),
            g300: hex("#e0e0e0"),
            g400: hex("#bdbdbd"),
            g500: hex("#9e9e9e"),
            g600: hex("#757575"),
            g700: hex("#616161"),
            g800: hex("#424242"),
            g900: hex("#212121"),
        }
    }
}

/// Component-level tokens from theme.ts that are not palette entries.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Components {
    pub scroll_thumb: Hsla,
    pub scroll_thumb_hover: Hsla,
    pub tooltip_bg: Hsla,
    pub selected_pill: Hsla,
    pub selected_pill_hover: Hsla,
    /// MuiListItemButton hover.
    pub list_hover: Hsla,
    /// MuiMenu / MuiAutocomplete paper shadow color (0 8px 28px).
    pub menu_shadow: Hsla,
    /// `::selection` background.
    pub text_selection: Hsla,
    /// MuiOutlinedInput hover outline.
    pub input_hover_border: Hsla,
    /// Shadow of a floating client-decorated window (Chromium's frame).
    pub window_shadow: Hsla,
    /// The framed surface a session view sits on before the view paints its
    /// own (the default terminal schemes' background).
    pub session_surface: Hsla,
    /// A faint tile on a primary-filled card (the empty pane's main card).
    pub on_primary_tile: Hsla,
    /// Secondary text on a primary-filled card.
    pub on_primary_secondary: Hsla,
    /// The check mark on a selected color swatch.
    pub swatch_check: Hsla,
    /// The pane behind a remote desktop picture and its connecting and ended screens
    /// (`bgcolor: '#101014'` in RemoteDesktopViewImpl.tsx), the same in both modes.
    pub remote_desktop_bg: Hsla,
    /// Text and icons on those screens (`color: 'grey.300'`, MUI grey 300).
    pub remote_desktop_ink: Hsla,
    /// The code editor's surface (Monaco's `vs` / `vs-dark` editor background).
    pub code_surface: Hsla,
    /// A modal's backdrop (`MuiBackdrop`: 50% black).
    pub backdrop: Hsla,
    /// The scrim behind a drawer that floats over content (25% black).
    pub scrim: Hsla,
    /// The check mark on a selected host color swatch (`HostColorPicker`:
    /// 68% black, the same in both modes).
    pub host_swatch_check: Hsla,
    /// The 1px frame Chrome draws around an `<input type="color">` swatch.
    pub color_input_frame: Hsla,
    /// A hairline around a light preview tile (a light terminal scheme).
    pub outline_on_light: Hsla,
    /// A hairline around a dark preview tile (a dark terminal scheme).
    pub outline_on_dark: Hsla,
    /// The primary-tinted wash behind About's hero (`alpha(primary, …)` on paper).
    pub primary_wash: Hsla,
}

/// Color flags a tab can be marked with (`TAB_FLAG_COLORS` in TabStrip.tsx).
/// The value is also what a tab stores, so these stay CSS hex strings.
pub const TAB_FLAG_COLORS: [&str; 8] = [
    "#ef5350", "#ffa726", "#ffee58", "#66bb6a", "#26c6da", "#42a5f5", "#ab47bc", "#ec407a",
];

/// One MUI typography variant, sizes already scaled by the theme's coefficient.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextVariant {
    pub size: f32,
    pub weight: f32,
    /// Line height in px.
    pub line_height: f32,
    /// Letter spacing in px (GPUI cannot apply it yet; kept for reference).
    pub letter_spacing: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Typography {
    /// `typography.fontSize`.
    pub font_size: f32,
    pub html_font_size: f32,
    pub h5: TextVariant,
    pub h6: TextVariant,
    pub subtitle1: TextVariant,
    pub subtitle2: TextVariant,
    pub body1: TextVariant,
    pub body2: TextVariant,
    pub caption: TextVariant,
    pub button: TextVariant,
    pub overline: TextVariant,
    /// `chordSx()`: shortcut hints in the mono tier (11px, not rem-scaled).
    pub chord: TextVariant,
    /// `chordSx(10)`: the smaller chord tier (key hints, the launcher's chip).
    pub chord_small: TextVariant,
    /// The quick launcher's search input (`fontSize: 17`, line 1.4375em).
    pub search_input: TextVariant,
    /// Small caps result tags (`fontSize: 9.5`, 0.5px tracking, caption line).
    pub tag: TextVariant,
    /// Preformatted output in the system monospace face (`fontSize: 12`,
    /// `lineHeight: 1.5`: the session history transcript).
    pub code: TextVariant,
}

impl Typography {
    /// MUI `pxToRem` resolved back to px: `size / 14 * fontSize`.
    pub fn px_to_rem(&self, size: f32) -> f32 {
        size * self.font_size / 14.
    }

    fn new() -> Self {
        let coef = 13. / 14.;
        let variant = |size: f32, weight: f32, line_height: f32, letter_spacing_em: f32| {
            let size = size * coef;
            TextVariant {
                size,
                weight,
                line_height: size * line_height,
                letter_spacing: size * letter_spacing_em,
            }
        };
        Self {
            font_size: 13.,
            html_font_size: 16.,
            // theme.ts: h5/h6 600 with -0.2 tracking; subtitles 600; button 550.
            h5: TextVariant {
                letter_spacing: -0.2,
                ..variant(24., 600., 1.334, 0.)
            },
            h6: TextVariant {
                letter_spacing: -0.2,
                ..variant(20., 600., 1.6, 0.)
            },
            subtitle1: variant(16., 600., 1.75, 0.),
            subtitle2: variant(14., 600., 1.57, 0.),
            body1: variant(16., 400., 1.5, 0.),
            body2: variant(14., 400., 1.43, 0.),
            caption: variant(12., 400., 1.66, 0.),
            button: variant(14., 550., 1.75, 0.),
            overline: variant(12., 400., 2.66, 0.),
            chord: TextVariant {
                size: 11.,
                weight: 400.,
                line_height: 11. * 1.5,
                letter_spacing: 0.,
            },
            chord_small: TextVariant {
                size: 10.,
                weight: 400.,
                line_height: 10. * 1.5,
                letter_spacing: 0.,
            },
            search_input: TextVariant {
                size: 17.,
                weight: 400.,
                line_height: 17. * 1.4375,
                letter_spacing: 0.,
            },
            tag: TextVariant {
                size: 9.5,
                weight: 400.,
                line_height: 9.5 * 1.66,
                letter_spacing: 0.5,
            },
            code: TextVariant {
                size: 12.,
                weight: 400.,
                line_height: 18.,
                letter_spacing: 0.,
            },
        }
    }
}

/// The resolved theme, installed as a GPUI global by [`Theme::install`].
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Theme {
    pub mode: Mode,
    pub palette: Palette,
    pub components: Components,
    pub typography: Typography,
    /// `shape.borderRadius`.
    pub radius: f32,
}

impl Global for Theme {}

/// Font families. Chrome text uses the bundled Inter (static faces per
/// weight, see assets/fonts); terminals use JetBrains Mono.
pub mod fonts {
    pub const UI: &str = "Inter";
    pub const MONO: &str = "JetBrains Mono";
    pub const SYMBOLS: &str = "Pure Nerd Font";
    /// CSS `monospace`: what Chromium resolves it to through fontconfig on
    /// the usual Linux desktops (the session history transcript).
    pub const SYSTEM_MONO: &str = "DejaVu Sans Mono";
}

/// Font weights the client sets with `fontWeight` (Inter ships a static
/// face for each).
pub mod weight {
    pub const REGULAR: f32 = 400.;
    pub const MEDIUM: f32 = 500.;
    /// The theme's emphasis tier (buttons, folder rows).
    pub const EMPHASIS: f32 = 550.;
    pub const SEMIBOLD: f32 = 600.;
    /// Names in import and history lists.
    pub const STRONG: f32 = 650.;
    pub const BOLD: f32 = 700.;
}

/// Sizes the client sets in plain px (`sx={{ fontSize: 11 }}`), which MUI
/// does not scale with the theme's font size.
pub mod sizes {
    /// Chip labels in dense rows ("config").
    pub const TEXT_MICRO: f32 = 10.;
    /// Dense captions and monospace rule lines (forwarding panel, section labels).
    pub const TEXT_DENSE: f32 = 11.;
    /// Small monospace text (log lines, secondary values in menus).
    pub const TEXT_SMALL: f32 = 12.;
    /// Letter spacing of upper-case section labels.
    pub const OVERLINE_TRACKING: f32 = 0.6;
    /// Inline marker icons beside a name.
    pub const ICON_TINY: f32 = 13.;
    pub const ICON_COMPACT: f32 = 14.;
    /// Icons of compact icon buttons in dense rows.
    pub const ICON_DENSE: f32 = 16.;
    /// Chip start icons and search adornments.
    pub const ICON_CHIP: f32 = 18.;
}

struct Colors {
    primary: &'static str,
    secondary: &'static str,
    bg_default: &'static str,
    bg_paper: &'static str,
    sidebar: &'static str,
    sidebar_ink: &'static str,
    divider: Hsla,
    text_primary: &'static str,
    text_secondary: &'static str,
    success: &'static str,
    warning: &'static str,
    error: &'static str,
    info: &'static str,
    scroll_thumb: Hsla,
    scroll_thumb_hover: Hsla,
    tooltip_bg: &'static str,
    selected_pill: Hsla,
    selected_pill_hover: Hsla,
}

fn dark_colors() -> Colors {
    Colors {
        primary: "#6e8bfb",
        secondary: "#2dd4bf",
        bg_default: "#1b1b1f",
        bg_paper: "#232328",
        sidebar: "#151518",
        sidebar_ink: "#e6e6ea",
        divider: rgba8(255, 255, 255, 0.08),
        text_primary: "#e6e6ea",
        text_secondary: "#9d9da7",
        success: "#4ade80",
        warning: "#e7b341",
        error: "#f87171",
        info: "#60a5fa",
        scroll_thumb: rgba8(255, 255, 255, 0.16),
        scroll_thumb_hover: rgba8(255, 255, 255, 0.30),
        tooltip_bg: "#2e2e34",
        selected_pill: rgba8(255, 255, 255, 0.09),
        selected_pill_hover: rgba8(255, 255, 255, 0.13),
    }
}

fn light_colors() -> Colors {
    Colors {
        primary: "#3b66f5",
        secondary: "#0d9488",
        bg_default: "#fafafa",
        bg_paper: "#ffffff",
        sidebar: "#f4f4f5",
        sidebar_ink: "#3f3f49",
        divider: rgba8(0, 0, 0, 0.08),
        text_primary: "#1c1c21",
        text_secondary: "#6e6e78",
        success: "#16a34a",
        warning: "#b07a10",
        error: "#dc2626",
        info: "#2563eb",
        scroll_thumb: rgba8(0, 0, 0, 0.18),
        scroll_thumb_hover: rgba8(0, 0, 0, 0.34),
        tooltip_bg: "#2e2e34",
        selected_pill: rgba8(0, 0, 0, 0.07),
        selected_pill_hover: rgba8(0, 0, 0, 0.10),
    }
}

impl Theme {
    /// `buildTheme(mode)`.
    pub fn new(mode: Mode) -> Self {
        let dark = mode.is_dark();
        let c = if dark { dark_colors() } else { light_colors() };
        // Light mode pins the `dark` variants to hues that pass WCAG AA as
        // small text on the near-white backgrounds (see status_text_color).
        let pinned = |value: &str| if dark { None } else { Some(hex(value)) };
        let text_secondary = hex(c.text_secondary);
        let action = if dark {
            ActionColors {
                active: hex("#fff"),
                hover: rgba8(255, 255, 255, 0.08),
                selected: rgba8(255, 255, 255, 0.16),
                disabled: rgba8(255, 255, 255, 0.3),
                disabled_background: rgba8(255, 255, 255, 0.12),
                focus: rgba8(255, 255, 255, 0.12),
                hover_opacity: 0.08,
                selected_opacity: 0.16,
            }
        } else {
            ActionColors {
                active: rgba8(0, 0, 0, 0.54),
                hover: rgba8(0, 0, 0, 0.04),
                selected: rgba8(0, 0, 0, 0.08),
                disabled: rgba8(0, 0, 0, 0.26),
                disabled_background: rgba8(0, 0, 0, 0.12),
                focus: rgba8(0, 0, 0, 0.12),
                hover_opacity: 0.04,
                selected_opacity: 0.08,
            }
        };
        let primary = hex(c.primary);
        let palette = Palette {
            mode,
            primary: PaletteColor::augment(primary, None),
            secondary: PaletteColor::augment(hex(c.secondary), None),
            success: PaletteColor::augment(hex(c.success), pinned("#15803d")),
            warning: PaletteColor::augment(hex(c.warning), pinned("#8f6209")),
            error: PaletteColor::augment(hex(c.error), pinned("#b91c1c")),
            info: PaletteColor::augment(hex(c.info), pinned("#1d4ed8")),
            divider: c.divider,
            sidebar: hex(c.sidebar),
            sidebar_ink: hex(c.sidebar_ink),
            background_default: hex(c.bg_default),
            background_paper: hex(c.bg_paper),
            text: TextColors {
                primary: hex(c.text_primary),
                secondary: text_secondary,
                disabled: alpha(text_secondary, if dark { 0.55 } else { 0.6 }),
            },
            action,
            common_white: hex("#fff"),
            common_black: hex("#000"),
            grey: Grey::mui(),
        };
        let components = Components {
            scroll_thumb: c.scroll_thumb,
            scroll_thumb_hover: c.scroll_thumb_hover,
            tooltip_bg: hex(c.tooltip_bg),
            selected_pill: c.selected_pill,
            selected_pill_hover: c.selected_pill_hover,
            list_hover: if dark {
                rgba8(255, 255, 255, 0.05)
            } else {
                rgba8(0, 0, 0, 0.04)
            },
            menu_shadow: if dark {
                rgba8(0, 0, 0, 0.5)
            } else {
                rgba8(0, 0, 0, 0.12)
            },
            text_selection: alpha(primary, 0.3),
            input_hover_border: alpha(primary, 0.5),
            window_shadow: rgba8(0, 0, 0, 0.18),
            session_surface: if dark { hex("#1e1e1e") } else { hex("#ffffff") },
            on_primary_tile: rgba8(255, 255, 255, 0.16),
            on_primary_secondary: rgba8(255, 255, 255, 0.78),
            swatch_check: rgba8(0, 0, 0, 0.7),
            remote_desktop_bg: hex("#101014"),
            remote_desktop_ink: hex("#e0e0e0"),
            code_surface: if dark { hex("#1e1e1e") } else { hex("#ffffff") },
            backdrop: rgba8(0, 0, 0, 0.5),
            scrim: rgba8(0, 0, 0, 0.25),
            host_swatch_check: rgba8(0, 0, 0, 0.68),
            color_input_frame: hex("#777777"),
            outline_on_light: rgba8(0, 0, 0, 0.14),
            outline_on_dark: rgba8(255, 255, 255, 0.14),
            primary_wash: if dark { hex("#2a2d45") } else { hex("#eef2ff") },
        };
        Self {
            mode,
            palette,
            components,
            typography: Typography::new(),
            radius: 8.,
        }
    }

    /// MUI `theme.spacing(n)`: 8px steps.
    pub fn spacing(n: f32) -> Pixels {
        px(8. * n)
    }

    /// `theme.spacing(n)` as a CSS number, for APIs that take CSS lengths as `f32`.
    pub fn spacing_css(n: f32) -> f32 {
        8. * n
    }

    /// `statusTextColor`: the bright palette mains pass contrast on dark paper
    /// but fail WCAG AA as 12–13px text on the light backgrounds, so light mode
    /// drops to the pinned `dark` variant.
    pub fn status_text_color(&self, status: Status) -> Hsla {
        let color = match status {
            Status::Success => self.palette.success,
            Status::Error => self.palette.error,
            Status::Warning => self.palette.warning,
            Status::Info => self.palette.info,
        };
        if self.mode.is_dark() {
            color.main
        } else {
            color.dark
        }
    }

    /// Colors the window controls are painted with (`titleBarColors`): they
    /// must match the rendered top bar, which uses `sidebar` as background.
    /// `dim` composites the modal backdrop over them during a dialog's fade.
    pub fn title_bar_colors(&self, dim: f32) -> (Hsla, Hsla) {
        let factor = 1. - 0.5 * dim.clamp(0., 1.);
        let scale = |color: Hsla| {
            let c = color.to_rgb();
            gpui_kit::Rgba {
                r: c.r * factor,
                g: c.g * factor,
                b: c.b * factor,
                a: 1.,
            }
            .into()
        };
        if dim <= 0. {
            (self.palette.sidebar, self.palette.text.primary)
        } else {
            (
                scale(self.palette.sidebar),
                scale(self.palette.text.primary),
            )
        }
    }

    /// Install (or replace) the global theme and refresh every window.
    pub fn install(mode: Mode, cx: &mut App) {
        cx.set_global(Theme::new(mode));
        // The component layer's own widgets (the code editor's gutter and
        // current-line band, scrollbars) follow the same mode.
        gpui_kit::component::Theme::change(
            if mode.is_dark() {
                gpui_kit::component::ThemeMode::Dark
            } else {
                gpui_kit::component::ThemeMode::Light
            },
            None,
            cx,
        );
        cx.refresh_windows();
    }

    pub fn global(cx: &App) -> &Theme {
        cx.global::<Theme>()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Status {
    Success,
    Error,
    Warning,
    Info,
}

/// Shorthand for reading the installed theme.
pub trait ActiveTheme {
    fn theme(&self) -> &Theme;
}

impl ActiveTheme for App {
    fn theme(&self) -> &Theme {
        Theme::global(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn css(color: Hsla) -> String {
        let c = color.to_rgb();
        format!(
            "rgba({}, {}, {}, {:.2})",
            (c.r * 255.).round(),
            (c.g * 255.).round(),
            (c.b * 255.).round(),
            c.a
        )
    }

    #[test]
    fn light_palette_matches_theme_ts() {
        let theme = Theme::new(Mode::Light);
        assert_eq!(css(theme.palette.sidebar), "rgba(244, 244, 245, 1.00)");
        assert_eq!(css(theme.palette.sidebar_ink), "rgba(63, 63, 73, 1.00)");
        assert_eq!(
            css(theme.palette.text.disabled),
            "rgba(110, 110, 120, 0.60)"
        );
        assert_eq!(
            css(theme.palette.primary.contrast_text),
            "rgba(255, 255, 255, 1.00)"
        );
        assert_eq!(
            css(theme.status_text_color(Status::Success)),
            "rgba(21, 128, 61, 1.00)"
        );
    }

    #[test]
    fn dark_palette_uses_mains_for_status_text() {
        let theme = Theme::new(Mode::Dark);
        assert_eq!(
            css(theme.status_text_color(Status::Warning)),
            "rgba(231, 179, 65, 1.00)"
        );
        assert_eq!(
            css(theme.palette.text.disabled),
            "rgba(157, 157, 167, 0.55)"
        );
    }

    #[test]
    fn typography_scales_by_font_size() {
        let theme = Theme::new(Mode::Light);
        assert!((theme.typography.body2.size - 13.).abs() < 1e-4);
        assert!((theme.typography.h6.size - 18.5714).abs() < 1e-3);
        assert!((theme.typography.px_to_rem(20.) - 18.5714).abs() < 1e-3);
    }

    #[test]
    fn title_bar_dims_with_the_backdrop() {
        let theme = Theme::new(Mode::Light);
        let (bar, _) = theme.title_bar_colors(1.);
        assert_eq!(css(bar), "rgba(122, 122, 123, 1.00)");
    }
}
