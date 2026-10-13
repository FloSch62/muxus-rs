//! Colors as the terminal renders them: CSS color parsing, blending and the WCAG contrast
//! adjustment xterm applies to every rendered text cell (`minimumContrastRatio`).
//!
//! The contrast helpers are adapted from xterm.js `src/common/Color.ts` (MIT).

/// An sRGB color with 8-bit channels and an 8-bit alpha (255 is opaque).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Rgba {
    pub r: u8,
    pub g: u8,
    pub b: u8,
    pub a: u8,
}

impl Rgba {
    pub const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    pub const fn with_alpha(self, a: u8) -> Self {
        Self { a, ..self }
    }

    /// `0xRRGGBB`, dropping alpha.
    pub const fn to_rgb_u32(self) -> u32 {
        ((self.r as u32) << 16) | ((self.g as u32) << 8) | self.b as u32
    }

    pub const fn from_rgb_u32(value: u32) -> Self {
        Self::rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
    }

    /// `#rrggbb`, dropping alpha.
    pub fn to_hex(self) -> String {
        format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
    }

    /// This color painted over an opaque `backdrop`, as xterm's `rgba.blend`.
    pub fn over(self, backdrop: Rgba) -> Rgba {
        if self.a == 255 {
            return self;
        }
        let alpha = f64::from(self.a) / 255.0;
        let mix = |top: u8, bottom: u8| -> u8 {
            (f64::from(bottom) + ((f64::from(top) - f64::from(bottom)) * alpha).round()) as u8
        };
        Rgba::rgb(
            mix(self.r, backdrop.r),
            mix(self.g, backdrop.g),
            mix(self.b, backdrop.b),
        )
    }

    /// Half the opacity, which is how xterm paints dim backgrounds.
    pub fn multiply_opacity(self, factor: f64) -> Rgba {
        let a = (f64::from(self.a) * factor).round().clamp(0.0, 255.0) as u8;
        self.with_alpha(a)
    }

    /// WCAG relative luminance (xterm's `rgb.relativeLuminance2`).
    pub fn relative_luminance(self) -> f64 {
        relative_luminance(self.r, self.g, self.b)
    }
}

/// Parse a CSS color the way terminal themes write them: `#rgb`, `#rrggbb`, `#rrggbbaa`,
/// `rgb(r, g, b)` and `rgba(r, g, b, a)` with `a` in 0–1.
pub fn parse_css_color(value: &str) -> Option<Rgba> {
    let text = value.trim();
    if let Some(hex) = text.strip_prefix('#') {
        if !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        let digits: String = match hex.len() {
            3 | 4 => hex.chars().flat_map(|c| [c, c]).collect(),
            6 | 8 => hex.to_string(),
            _ => return None,
        };
        let channel = |offset: usize| u8::from_str_radix(&digits[offset..offset + 2], 16).ok();
        return Some(Rgba {
            r: channel(0)?,
            g: channel(2)?,
            b: channel(4)?,
            a: if digits.len() == 8 { channel(6)? } else { 255 },
        });
    }
    let lower = text.to_ascii_lowercase();
    let inner = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?
        .strip_suffix(')')?;
    let parts: Vec<&str> = inner.split(',').map(str::trim).collect();
    if parts.len() != 3 && parts.len() != 4 {
        return None;
    }
    let channel = |part: &str| -> Option<u8> {
        let value: f64 = part.parse().ok()?;
        (0.0..=255.0).contains(&value).then(|| value.round() as u8)
    };
    let alpha = match parts.get(3) {
        Some(part) => {
            let value: f64 = part.parse().ok()?;
            (value.clamp(0.0, 1.0) * 255.0).round() as u8
        }
        None => 255,
    };
    Some(Rgba {
        r: channel(parts[0])?,
        g: channel(parts[1])?,
        b: channel(parts[2])?,
        a: alpha,
    })
}

/// WCAG relative luminance of an sRGB color.
pub fn relative_luminance(r: u8, g: u8, b: u8) -> f64 {
    let linear = |channel: u8| {
        let value = f64::from(channel) / 255.0;
        if value <= 0.03928 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    linear(r) * 0.2126 + linear(g) * 0.7152 + linear(b) * 0.0722
}

/// Contrast ratio of two relative luminances.
pub fn contrast_ratio(l1: f64, l2: f64) -> f64 {
    if l1 < l2 {
        (l2 + 0.05) / (l1 + 0.05)
    } else {
        (l1 + 0.05) / (l2 + 0.05)
    }
}

fn reduce_luminance(bg: Rgba, fg: Rgba, ratio: f64) -> Rgba {
    let bg_l = bg.relative_luminance();
    let (mut r, mut g, mut b) = (i32::from(fg.r), i32::from(fg.g), i32::from(fg.b));
    let lum = |r: i32, g: i32, b: i32| relative_luminance(r as u8, g as u8, b as u8);
    let mut cr = contrast_ratio(lum(r, g, b), bg_l);
    while cr < ratio && (r > 0 || g > 0 || b > 0) {
        // Reduce by 10% until the ratio is hit.
        r -= ((f64::from(r) * 0.1).ceil() as i32).max(0);
        g -= ((f64::from(g) * 0.1).ceil() as i32).max(0);
        b -= ((f64::from(b) * 0.1).ceil() as i32).max(0);
        cr = contrast_ratio(lum(r, g, b), bg_l);
    }
    Rgba::rgb(r as u8, g as u8, b as u8)
}

fn increase_luminance(bg: Rgba, fg: Rgba, ratio: f64) -> Rgba {
    let bg_l = bg.relative_luminance();
    let (mut r, mut g, mut b) = (i32::from(fg.r), i32::from(fg.g), i32::from(fg.b));
    let lum = |r: i32, g: i32, b: i32| relative_luminance(r as u8, g as u8, b as u8);
    let mut cr = contrast_ratio(lum(r, g, b), bg_l);
    while cr < ratio && (r < 255 || g < 255 || b < 255) {
        // Increase by 10% until the ratio is hit.
        r = (r + (f64::from(255 - r) * 0.1).ceil() as i32).min(255);
        g = (g + (f64::from(255 - g) * 0.1).ceil() as i32).min(255);
        b = (b + (f64::from(255 - b) * 0.1).ceil() as i32).min(255);
        cr = contrast_ratio(lum(r, g, b), bg_l);
    }
    Rgba::rgb(r as u8, g as u8, b as u8)
}

/// Lighten or darken `fg` until it reaches `ratio` against `bg`; `None` when it already does.
/// When pure white or black is hit first, the other direction is tried and the better of the two
/// is used — xterm's `rgba.ensureContrastRatio`.
pub fn ensure_contrast_ratio(bg: Rgba, fg: Rgba, ratio: f64) -> Option<Rgba> {
    let bg_l = bg.relative_luminance();
    let fg_l = fg.relative_luminance();
    if contrast_ratio(bg_l, fg_l) >= ratio {
        return None;
    }
    let ratio_of = |color: Rgba| contrast_ratio(bg_l, color.relative_luminance());
    type Step = fn(Rgba, Rgba, f64) -> Rgba;
    let (first, second): (Step, Step) = if fg_l < bg_l {
        (reduce_luminance, increase_luminance)
    } else {
        (increase_luminance, reduce_luminance)
    };
    let result_a = first(bg, fg, ratio);
    let ratio_a = ratio_of(result_a);
    if ratio_a < ratio {
        let result_b = second(bg, fg, ratio);
        let ratio_b = ratio_of(result_b);
        return Some(if ratio_a > ratio_b {
            result_a
        } else {
            result_b
        });
    }
    Some(result_a)
}

/// The xterm 256-color table entry for indices 16–255 (6×6×6 cube, then the gray ramp).
pub fn xterm_256_color(index: u8) -> Rgba {
    if index < 16 {
        // Callers resolve the first 16 through the scheme.
        return Rgba::rgb(0, 0, 0);
    }
    if index >= 232 {
        let level = 8 + (index - 232) * 10;
        return Rgba::rgb(level, level, level);
    }
    const STEPS: [u8; 6] = [0x00, 0x5f, 0x87, 0xaf, 0xd7, 0xff];
    let cube = index - 16;
    Rgba::rgb(
        STEPS[(cube / 36) as usize],
        STEPS[((cube / 6) % 6) as usize],
        STEPS[(cube % 6) as usize],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_css_notations() {
        assert_eq!(
            parse_css_color("#181818"),
            Some(Rgba::rgb(0x18, 0x18, 0x18))
        );
        assert_eq!(parse_css_color("#fff"), Some(Rgba::rgb(255, 255, 255)));
        assert_eq!(
            parse_css_color("#00000000"),
            Some(Rgba {
                r: 0,
                g: 0,
                b: 0,
                a: 0
            })
        );
        assert_eq!(
            parse_css_color("rgba(110, 139, 251, 0.32)"),
            Some(Rgba {
                r: 110,
                g: 139,
                b: 251,
                a: 82
            })
        );
        assert_eq!(parse_css_color("rgb(1, 2, 3)"), Some(Rgba::rgb(1, 2, 3)));
        assert_eq!(parse_css_color("orange"), None);
        assert_eq!(parse_css_color("#12"), None);
    }

    #[test]
    fn blends_translucent_colors_over_the_backdrop() {
        let selection = parse_css_color("rgba(88, 91, 112, 0.6)").unwrap();
        let background = parse_css_color("#1e1e2e").unwrap();
        // xterm rounds the alpha to 8 bits first; the result stays within a channel step.
        let blended = selection.over(background);
        assert!((i32::from(blended.r) - 0x41).abs() <= 1);
        assert!((i32::from(blended.b) - 0x56).abs() <= 1);
    }

    #[test]
    fn leaves_sufficient_contrast_alone_and_fixes_the_rest() {
        let black = Rgba::rgb(0, 0, 0);
        assert_eq!(
            ensure_contrast_ratio(black, Rgba::rgb(255, 255, 255), 4.5),
            None
        );
        let adjusted = ensure_contrast_ratio(black, Rgba::rgb(0x20, 0x20, 0x20), 4.5).unwrap();
        let ratio = contrast_ratio(black.relative_luminance(), adjusted.relative_luminance());
        assert!(ratio >= 4.5);
    }

    #[test]
    fn builds_the_256_color_cube() {
        assert_eq!(xterm_256_color(16), Rgba::rgb(0, 0, 0));
        assert_eq!(xterm_256_color(196), Rgba::rgb(0xff, 0, 0));
        assert_eq!(xterm_256_color(232), Rgba::rgb(8, 8, 8));
        assert_eq!(xterm_256_color(255), Rgba::rgb(238, 238, 238));
    }
}
