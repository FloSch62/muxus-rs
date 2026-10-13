//! Names for terminal exports (`exportFilename` in `client/src/save-file.ts`).

/// `muxus-<slug>-YYYYMMDD-HHMMSS.<ext>` in local time, the slug from the tab title.
pub fn export_filename(title: &str, ext: &str) -> String {
    export_filename_at(title, ext, chrono::Local::now().naive_local())
}

/// [`export_filename`] at a given local time.
pub fn export_filename_at(title: &str, ext: &str, now: chrono::NaiveDateTime) -> String {
    let lowered = title.to_lowercase();
    let mut slug = String::new();
    let mut gap = false;
    for character in lowered.chars() {
        if character.is_ascii_lowercase() || character.is_ascii_digit() {
            if gap && !slug.is_empty() {
                slug.push('-');
            }
            gap = false;
            slug.push(character);
        } else {
            gap = true;
        }
    }
    // `.slice(0, 40)` after trimming the dashes; a cut may end on a dash.
    let mut slug: String = slug.chars().take(40).collect();
    if slug.is_empty() {
        slug = "terminal".into();
    }
    format!("muxus-{slug}-{}.{ext}", now.format("%Y%m%d-%H%M%S"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> chrono::NaiveDateTime {
        chrono::NaiveDate::from_ymd_opt(2026, 3, 4)
            .unwrap()
            .and_hms_opt(5, 6, 7)
            .unwrap()
    }

    #[test]
    fn slugs_the_title_and_stamps_the_time() {
        assert_eq!(
            export_filename_at("web-01 (prod)", "txt", at()),
            "muxus-web-01-prod-20260304-050607.txt"
        );
        assert_eq!(
            export_filename_at("  ", "html", at()),
            "muxus-terminal-20260304-050607.html"
        );
        assert_eq!(
            export_filename_at("Ünïcode only é", "txt", at()),
            "muxus-n-code-only-20260304-050607.txt"
        );
    }
}
