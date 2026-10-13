//! Terminal font families offered in settings (port of `client/src/terminal/font-catalog.ts`)
//! and the fallback stack behind the chosen family (`terminalFontStack` in
//! `client/src/state/prefs.ts`).

pub const BUNDLED_TERMINAL_FONT_FAMILIES: [&str; 1] = ["JetBrains Mono"];
pub const GENERIC_TERMINAL_FONT_FAMILIES: [&str; 1] = ["monospace"];

/// Bundled icon-only fallback covering Nerd Font and Powerline glyphs.
pub const TERMINAL_SYMBOL_FONT: &str = "Pure Nerd Font";

/// Families tried after the user's choice, in order.
pub const MONO_FONT_FALLBACK: [&str; 6] = [
    "JetBrains Mono",
    TERMINAL_SYMBOL_FONT,
    "Noto Sans Mono",
    "DejaVu Sans Mono",
    "Liberation Mono",
    "monospace",
];

const CSS_GENERIC_FONT_FAMILIES: [&str; 13] = [
    "serif",
    "sans-serif",
    "monospace",
    "cursive",
    "fantasy",
    "system-ui",
    "ui-serif",
    "ui-sans-serif",
    "ui-monospace",
    "ui-rounded",
    "emoji",
    "math",
    "fangsong",
];

const MAX_FONT_FAMILY_LENGTH: usize = 200;

fn unquote(family: &str) -> &str {
    let value = family.trim();
    if value.len() >= 2
        && ((value.starts_with('"') && value.ends_with('"'))
            || (value.starts_with('\'') && value.ends_with('\'')))
    {
        return value[1..value.len() - 1].trim();
    }
    value
}

fn normalized_family(family: &str) -> String {
    unquote(family).to_lowercase()
}

fn is_css_generic_family(family: &str) -> bool {
    let trimmed = family.trim();
    if (trimmed.starts_with('"') && trimmed.ends_with('"'))
        || (trimmed.starts_with('\'') && trimmed.ends_with('\''))
    {
        return false;
    }
    CSS_GENERIC_FONT_FAMILIES.contains(&trimmed.to_lowercase().as_str())
}

/// Bundled, installed and generic terminal fonts without case-only duplicates.
pub fn terminal_font_families(installed: &[&str]) -> Vec<String> {
    let mut families: Vec<(String, String)> = Vec::new();
    let add = |family: &str, families: &mut Vec<(String, String)>| {
        let trimmed = family.trim();
        if trimmed.is_empty() || trimmed.encode_utf16().count() > MAX_FONT_FAMILY_LENGTH {
            return;
        }
        let key = normalized_family(trimmed);
        match families.iter_mut().find(|(existing, _)| *existing == key) {
            Some(entry) => entry.1 = trimmed.to_string(),
            None => families.push((key, trimmed.to_string())),
        }
    };
    for family in BUNDLED_TERMINAL_FONT_FAMILIES {
        add(family, &mut families);
    }
    let mut sorted: Vec<&str> = installed.to_vec();
    sorted.sort_by(|left, right| {
        left.to_lowercase()
            .cmp(&right.to_lowercase())
            .then(left.cmp(right))
    });
    for family in sorted {
        let key = normalized_family(family);
        if !families.iter().any(|(existing, _)| *existing == key) {
            add(family, &mut families);
        }
    }
    for family in GENERIC_TERMINAL_FONT_FAMILIES {
        add(family, &mut families);
    }
    families.into_iter().map(|(_, family)| family).collect()
}

/// `None` means the platform could not enumerate local fonts.
pub fn terminal_font_is_available(family: &str, installed: Option<&[&str]>) -> Option<bool> {
    let selected = normalized_family(family);
    if selected.is_empty() {
        return Some(true);
    }
    if BUNDLED_TERMINAL_FONT_FAMILIES
        .iter()
        .any(|candidate| normalized_family(candidate) == selected)
        || is_css_generic_family(family)
    {
        return Some(true);
    }
    let installed = installed?;
    Some(
        installed
            .iter()
            .any(|candidate| normalized_family(candidate) == selected),
    )
}

/// The families to try for the `fontFamily` preference, first match wins: the user's family, then
/// the bundled text and symbol faces and common system monospace fonts.
pub fn terminal_font_stack(family: &str) -> Vec<String> {
    let trimmed = unquote(family);
    let mut stack = Vec::new();
    if !trimmed.is_empty()
        && !trimmed.eq_ignore_ascii_case("JetBrains Mono")
        && trimmed != "monospace"
    {
        stack.push(trimmed.to_string());
    }
    stack.extend(MONO_FONT_FALLBACK.iter().map(|family| family.to_string()));
    stack
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combines_bundled_installed_and_generic_families_without_duplicates() {
        assert_eq!(
            terminal_font_families(&[
                "Ubuntu Mono",
                "DejaVu Sans Mono",
                "jetbrains mono",
                " DejaVu Sans Mono ",
                ""
            ]),
            [
                "JetBrains Mono",
                "DejaVu Sans Mono",
                "Ubuntu Mono",
                "monospace"
            ]
        );
    }

    #[test]
    fn reports_unavailable_selections_only_when_enumeration_succeeded() {
        assert_eq!(
            terminal_font_is_available("JetBrains Mono", Some(&[])),
            Some(true)
        );
        assert_eq!(
            terminal_font_is_available("\"DejaVu Sans Mono\"", Some(&["DejaVu Sans Mono"])),
            Some(true)
        );
        assert_eq!(
            terminal_font_is_available("DejaVu Sans Mono", Some(&[])),
            Some(false)
        );
        assert_eq!(terminal_font_is_available("DejaVu Sans Mono", None), None);
    }

    #[test]
    fn recognizes_css_generic_families_without_requiring_local_enumeration() {
        for family in CSS_GENERIC_FONT_FAMILIES {
            assert_eq!(
                terminal_font_is_available(family, Some(&[])),
                Some(true),
                "{family}"
            );
        }
        assert_eq!(
            terminal_font_is_available("UI-MONOSPACE", Some(&[])),
            Some(true)
        );
        assert_eq!(
            terminal_font_is_available("\"ui-monospace\"", Some(&[])),
            Some(false)
        );
    }

    #[test]
    fn puts_the_chosen_family_before_the_bundled_fallbacks() {
        assert_eq!(terminal_font_stack("JetBrains Mono")[0], "JetBrains Mono");
        assert_eq!(
            terminal_font_stack("JetBrains Mono").len(),
            MONO_FONT_FALLBACK.len()
        );
        let stack = terminal_font_stack("Fira Code");
        assert_eq!(stack[0], "Fira Code");
        assert_eq!(stack[2], TERMINAL_SYMBOL_FONT);
        assert_eq!(terminal_font_stack("")[0], "JetBrains Mono");
    }
}
