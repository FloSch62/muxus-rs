//! Color helpers: CSS-style hex parsing and MUI's `colorManipulator`
//! (`alpha`, `lighten`, `darken`, `getContrastRatio`), so derived palette
//! entries come out exactly as MUI computes them.

use gpui_kit::{Hsla, Rgba};

/// `#rgb`, `#rrggbb` or `#rrggbbaa`. Anything else is opaque magenta, loud on purpose.
pub fn hex(value: &str) -> Hsla {
    parse_hex(value)
        .unwrap_or(Rgba {
            r: 1.,
            g: 0.,
            b: 1.,
            a: 1.,
        })
        .into()
}

/// Like [`hex`], but `None` for anything that is not a hex color.
pub fn try_hex(value: &str) -> Option<Hsla> {
    parse_hex(value).map(Into::into)
}

fn parse_hex(value: &str) -> Option<Rgba> {
    let digits = value.trim().strip_prefix('#')?;
    let channel = |s: &str| u8::from_str_radix(s, 16).ok().map(|v| v as f32 / 255.);
    let expand = |c: char| {
        let s: String = [c, c].iter().collect();
        channel(&s)
    };
    let mut chars = digits.chars();
    match digits.len() {
        3 => Some(Rgba {
            r: expand(chars.next()?)?,
            g: expand(chars.next()?)?,
            b: expand(chars.next()?)?,
            a: 1.,
        }),
        6 | 8 => Some(Rgba {
            r: channel(&digits[0..2])?,
            g: channel(&digits[2..4])?,
            b: channel(&digits[4..6])?,
            a: if digits.len() == 8 {
                channel(&digits[6..8])?
            } else {
                1.
            },
        }),
        _ => None,
    }
}

/// `rgba(r, g, b, a)` with 0–255 channels, as CSS writes it.
pub fn rgba8(r: u8, g: u8, b: u8, a: f32) -> Hsla {
    Rgba {
        r: r as f32 / 255.,
        g: g as f32 / 255.,
        b: b as f32 / 255.,
        a,
    }
    .into()
}

/// MUI `alpha(color, value)`: the same color at an absolute opacity.
pub fn alpha(color: Hsla, value: f32) -> Hsla {
    Hsla {
        a: value.clamp(0., 1.),
        ..color
    }
}

/// MUI `lighten(color, coefficient)`: each channel moves toward white.
pub fn lighten(color: Hsla, coefficient: f32) -> Hsla {
    let c = color.to_rgb();
    Rgba {
        r: c.r + (1. - c.r) * coefficient,
        g: c.g + (1. - c.g) * coefficient,
        b: c.b + (1. - c.b) * coefficient,
        a: c.a,
    }
    .into()
}

/// MUI `darken(color, coefficient)`: each channel scales toward black.
pub fn darken(color: Hsla, coefficient: f32) -> Hsla {
    let c = color.to_rgb();
    Rgba {
        r: c.r * (1. - coefficient),
        g: c.g * (1. - coefficient),
        b: c.b * (1. - coefficient),
        a: c.a,
    }
    .into()
}

/// WCAG relative luminance, as MUI's `getLuminance`.
pub fn luminance(color: Hsla) -> f32 {
    let c = color.to_rgb();
    let channel = |v: f32| {
        if v <= 0.03928 {
            v / 12.92
        } else {
            ((v + 0.055) / 1.055).powf(2.4)
        }
    };
    0.2126 * channel(c.r) + 0.7152 * channel(c.g) + 0.0722 * channel(c.b)
}

/// MUI `getContrastRatio`.
pub fn contrast_ratio(a: Hsla, b: Hsla) -> f32 {
    let (la, lb) = (luminance(a), luminance(b));
    (la.max(lb) + 0.05) / (la.min(lb) + 0.05)
}

/// Composite `top` over an opaque `bottom`, for places that need a solid color
/// equal to what a translucent layer looks like on a known background.
pub fn over(top: Hsla, bottom: Hsla) -> Hsla {
    let t = top.to_rgb();
    let b = bottom.to_rgb();
    let a = t.a;
    Rgba {
        r: t.r * a + b.r * (1. - a),
        g: t.g * a + b.g * (1. - a),
        b: t.b * a + b.b * (1. - a),
        a: 1.,
    }
    .into()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb8(color: Hsla) -> (u8, u8, u8) {
        let c = color.to_rgb();
        (
            (c.r * 255.).round() as u8,
            (c.g * 255.).round() as u8,
            (c.b * 255.).round() as u8,
        )
    }

    #[test]
    fn parses_hex_forms() {
        assert_eq!(rgb8(hex("#3b66f5")), (0x3b, 0x66, 0xf5));
        assert_eq!(rgb8(hex("#fff")), (255, 255, 255));
        assert!((hex("#00000080").a - 128. / 255.).abs() < 1e-6);
        assert!(try_hex("blue").is_none());
    }

    #[test]
    fn lighten_and_darken_match_mui() {
        // MUI: lighten('#3b66f5', 0.2) = rgb(98, 133, 247); darken(…, 0.3) = rgb(41, 71, 171)
        assert_eq!(rgb8(lighten(hex("#3b66f5"), 0.2)), (98, 133, 247));
        assert_eq!(rgb8(darken(hex("#3b66f5"), 0.3)), (41, 71, 172));
    }

    #[test]
    fn contrast_picks_white_on_primary() {
        assert!(contrast_ratio(hex("#3b66f5"), hex("#fff")) >= 3.);
        assert!(contrast_ratio(hex("#e7b341"), hex("#fff")) < 3.);
    }
}
