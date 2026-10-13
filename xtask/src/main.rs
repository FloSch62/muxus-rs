//! Workspace checks: `cargo xtask <command>`.
//!
//! - `layering`: every crate depends only on crates of lower layers
//!   (`docs/architecture.md`); L2 crates only on `muxus-core`.
//! - `style`: styling comes from the shared theme and widget kit. Fails on color literals
//!   (`"#rrggbb"`, `hex("…")`, `rgb(0x…)`, `rgba(0x…)`, `hsla(<number>…)`) in the UI crates
//!   outside the theme files, `palette.rs` files and tests, and on generic widget types
//!   (buttons, fields, switches, spinners, …) defined outside `muxus-ui`.
//! - `ci`: `cargo fmt --check`, `cargo clippy --workspace --all-targets -D warnings`,
//!   `cargo test --workspace`, then `layering` and `style`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("the workspace root")
        .to_path_buf()
}

/// The layer of each internal crate (`docs/architecture.md`).
fn layers() -> BTreeMap<&'static str, u8> {
    BTreeMap::from([
        ("muxus-core", 0),
        ("muxus-store", 1),
        ("muxus-ssh", 2),
        ("muxus-local", 2),
        ("muxus-telnet", 2),
        ("muxus-serial", 2),
        ("muxus-rdp", 2),
        ("muxus-vnc", 2),
        ("muxus-mgmt", 2),
        ("muxus-import", 2),
        ("muxus-term", 2),
        ("muxus-session", 3),
        ("muxus-ui", 4),
        ("muxus-term-view", 5),
        ("muxus-app", 6),
    ])
}

/// The internal crates a manifest depends on outside `[dev-dependencies]`.
fn internal_dependencies(manifest: &str) -> Vec<String> {
    let mut section = String::new();
    let mut found = Vec::new();
    for line in manifest.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            section = line.to_string();
            continue;
        }
        let normal = section == "[dependencies]"
            || section == "[build-dependencies]"
            || (section.starts_with("[target.") && section.ends_with(".dependencies]"));
        if !normal {
            continue;
        }
        if let Some(name) = line.split(['=', '.', ' ']).next()
            && name.starts_with("muxus-")
        {
            found.push(name.to_string());
        }
    }
    found
}

fn layering() -> Result<(), Vec<String>> {
    let layers = layers();
    let mut problems = Vec::new();
    for (name, layer) in &layers {
        let path = root().join("crates").join(name).join("Cargo.toml");
        let Ok(manifest) = std::fs::read_to_string(&path) else {
            problems.push(format!("{name}: no manifest at {}", path.display()));
            continue;
        };
        for dependency in internal_dependencies(&manifest) {
            let Some(dep_layer) = layers.get(dependency.as_str()) else {
                problems.push(format!(
                    "{name}: depends on unknown internal crate {dependency}"
                ));
                continue;
            };
            if dep_layer >= layer {
                problems.push(format!(
                    "{name} (L{layer}) depends on {dependency} (L{dep_layer}): only lower layers"
                ));
            } else if *layer == 2 && dependency != "muxus-core" {
                problems.push(format!(
                    "{name} (L2) depends on {dependency}: L2 crates depend on muxus-core only"
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

/// Every `.rs` file below `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Remove comments from one line, keeping string contents. `in_block` carries a `/* */`
/// comment across lines.
fn strip_comments(raw: &str, in_block: &mut bool) -> String {
    let mut code = String::new();
    let mut chars = raw.chars().peekable();
    let mut in_string = false;
    while let Some(c) = chars.next() {
        if *in_block {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                *in_block = false;
            }
            continue;
        }
        if in_string {
            code.push(c);
            if c == '\\' {
                if let Some(next) = chars.next() {
                    code.push(next);
                }
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_string = true;
                code.push(c);
            }
            '/' if chars.peek() == Some(&'/') => break,
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                *in_block = true;
            }
            _ => code.push(c),
        }
    }
    code
}

/// The lines of `source` that are code: comments removed, `#[cfg(test)]` items skipped.
/// Returns (line number, code) pairs.
fn code_lines(source: &str) -> Vec<(usize, String)> {
    let mut out = Vec::new();
    let mut in_block = false;
    let mut depth: i64 = 0;
    // The depth a skipped test item started at.
    let mut skip_from: Option<i64> = None;
    let mut pending_test = false;
    for (index, raw) in source.lines().enumerate() {
        let code = strip_comments(raw, &mut in_block);
        if code.trim().starts_with("#[cfg(test)]") {
            pending_test = true;
            continue;
        }
        let opens = code.matches('{').count() as i64;
        let closes = code.matches('}').count() as i64;
        if pending_test && skip_from.is_none() && opens > 0 {
            skip_from = Some(depth);
            pending_test = false;
        }
        let skipping = skip_from.is_some() || pending_test;
        depth += opens - closes;
        if let Some(start) = skip_from
            && depth <= start
        {
            skip_from = None;
        }
        if !skipping {
            out.push((index + 1, code));
        }
    }
    out
}

/// A color literal in a line of code.
fn color_literal(code: &str) -> Option<&'static str> {
    let bytes = code.as_bytes();
    for (i, _) in code.char_indices() {
        let rest = &code[i..];
        if let Some(after) = rest.strip_prefix("\"#") {
            let digits = after.chars().take_while(char::is_ascii_hexdigit).count();
            if after[digits..].starts_with('"') && matches!(digits, 3 | 4 | 6 | 8) {
                return Some("hex string");
            }
        }
        if rest.starts_with("\"rgb(") || rest.starts_with("\"rgba(") {
            return Some("rgb string");
        }
        let word_start = i == 0 || !(bytes[i - 1].is_ascii_alphanumeric() || bytes[i - 1] == b'_');
        if !word_start {
            continue;
        }
        if rest.starts_with("hex(\"") {
            return Some("hex()");
        }
        if rest.starts_with("rgb(0x") || rest.starts_with("rgba(0x") {
            return Some("rgb(0x…)");
        }
        if let Some(args) = rest.strip_prefix("hsla(")
            && args.starts_with(|c: char| c.is_ascii_digit() || c == '.')
        {
            return Some("hsla(<number>)");
        }
    }
    None
}

/// Names that are generic controls: they belong in `muxus-ui`.
const WIDGET_WORDS: [&str; 14] = [
    "Button",
    "TextField",
    "TextInput",
    "Select",
    "Switch",
    "Checkbox",
    "Radio",
    "Slider",
    "Chip",
    "Tooltip",
    "Spinner",
    "CircularProgress",
    "LinearProgress",
    "Badge",
];

const WIDGET_FNS: [&str; 10] = [
    "fn button(",
    "fn icon_button(",
    "fn text_field(",
    "fn checkbox(",
    "fn switch(",
    "fn spinner(",
    "fn chip(",
    "fn tooltip(",
    "fn select_field(",
    "fn progress(",
];

/// A generic widget type defined in a line of code.
fn widget_definition(code: &str) -> Option<String> {
    let trimmed = code.trim_start();
    let declaration = [
        "pub struct ",
        "struct ",
        "pub(crate) struct ",
        "pub enum ",
        "enum ",
    ]
    .iter()
    .find_map(|prefix| trimmed.strip_prefix(prefix));
    if let Some(rest) = declaration {
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if WIDGET_WORDS
            .iter()
            .any(|word| name == *word || name.ends_with(word))
        {
            return Some(name);
        }
    }
    WIDGET_FNS
        .iter()
        .find(|pattern| {
            trimmed.find(*pattern).is_some_and(|at| {
                // A method (`fn button(self)`) names a part of something, not a widget.
                let args = trimmed[at + pattern.len()..].trim_start();
                !(args.starts_with("self")
                    || args.starts_with("&self")
                    || args.starts_with("&mut self"))
            })
        })
        .map(|pattern| {
            pattern
                .trim_end_matches('(')
                .trim_start_matches("fn ")
                .to_string()
        })
}

/// Files allowed to hold color literals: the theme (with the code editors' syntax
/// theme), content palettes and test files.
fn colors_allowed(path: &Path) -> bool {
    let text = path.to_string_lossy().replace('\\', "/");
    path.file_name()
        .is_some_and(|name| name == "palette.rs" || name == "tests.rs")
        || text.contains("/muxus-ui/src/theme")
        || text.ends_with("/muxus-ui/src/color.rs")
        || text.ends_with("/muxus-ui/src/syntax.rs")
}

/// Names outside `muxus-ui` that look generic but are feature composites built from the kit
/// (a workbench chip preset, select fields wired to a form's state) or data (a tunnel's
/// start/stop switch).
const WIDGET_ALLOWLIST: &[&str] = &["chip", "select_field", "TunnelSwitch"];

fn style() -> Result<(), Vec<String>> {
    let mut problems = Vec::new();
    let mut files = Vec::new();
    for crate_name in ["muxus-ui", "muxus-term-view", "muxus-app"] {
        rust_files(
            &root().join("crates").join(crate_name).join("src"),
            &mut files,
        );
    }
    files.sort();
    for path in files {
        let Ok(source) = std::fs::read_to_string(&path) else {
            continue;
        };
        let relative = path
            .strip_prefix(root())
            .unwrap_or(&path)
            .to_string_lossy()
            .replace('\\', "/");
        let in_ui_kit = relative.starts_with("crates/muxus-ui/");
        for (line, code) in code_lines(&source) {
            if !colors_allowed(&path)
                && let Some(kind) = color_literal(&code)
            {
                problems.push(format!(
                    "{relative}:{line}: color literal ({kind}); use a theme token, or a palette.rs for content colors"
                ));
            }
            if !in_ui_kit
                && let Some(name) = widget_definition(&code)
                && !WIDGET_ALLOWLIST.contains(&name.as_str())
            {
                problems.push(format!(
                    "{relative}:{line}: generic widget `{name}` outside muxus-ui; add or extend it in crates/muxus-ui/src/widgets"
                ));
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

fn cargo(args: &[&str]) -> bool {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    eprintln!("$ cargo {}", args.join(" "));
    Command::new(cargo)
        .args(args)
        .current_dir(root())
        .status()
        .is_ok_and(|status| status.success())
}

fn report(name: &str, result: Result<(), Vec<String>>) -> bool {
    match result {
        Ok(()) => {
            eprintln!("{name}: ok");
            true
        }
        Err(problems) => {
            for problem in &problems {
                eprintln!("{problem}");
            }
            eprintln!("{name}: {} problem(s)", problems.len());
            false
        }
    }
}

fn main() -> ExitCode {
    let command = std::env::args().nth(1).unwrap_or_default();
    let ok = match command.as_str() {
        "layering" => report("layering", layering()),
        "style" => report("style", style()),
        "ci" => {
            cargo(&["fmt", "--all", "--check"])
                && cargo(&[
                    "clippy",
                    "--workspace",
                    "--all-targets",
                    "--",
                    "-D",
                    "warnings",
                ])
                && cargo(&["test", "--workspace"])
                && report("layering", layering())
                && report("style", style())
        }
        _ => {
            eprintln!("usage: cargo xtask <ci|layering|style>");
            false
        }
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_color_literals_in_code_only() {
        assert_eq!(color_literal(r##"let c = hex("#ffffff");"##), Some("hex()"));
        assert_eq!(
            color_literal(r##"color: Some("#3b82f6".into()),"##),
            Some("hex string")
        );
        assert_eq!(color_literal("rgb(0x1e1e1e)"), Some("rgb(0x…)"));
        assert_eq!(
            color_literal("hsla(0.5, 0.2, 0.1, 1.)"),
            Some("hsla(<number>)")
        );
        assert_eq!(color_literal("hsla(colors.background)"), None);
        assert_eq!(color_literal("issue #210"), None);
        let mut block = false;
        for line in ["// hex(\"#fff\")", "let a = 1; // \"#ffffff\""] {
            assert_eq!(color_literal(&strip_comments(line, &mut block)), None);
        }
    }

    #[test]
    fn skips_test_modules() {
        let source =
            "fn a() {}\n#[cfg(test)]\nmod tests {\n    fn b() { hex(\"#fff\"); }\n}\nfn c() {}\n";
        let lines: Vec<usize> = code_lines(source)
            .into_iter()
            .map(|(line, _)| line)
            .collect();
        assert_eq!(lines, vec![1, 6]);
    }

    #[test]
    fn finds_generic_widgets() {
        assert_eq!(
            widget_definition("pub struct SaveButton {"),
            Some("SaveButton".into())
        );
        assert_eq!(
            widget_definition("pub fn spinner(id: ElementId)"),
            Some("spinner".into())
        );
        assert_eq!(widget_definition("pub struct ForwardDiagram {"), None);
    }

    #[test]
    fn reads_internal_dependencies() {
        let manifest = "[dependencies]\nmuxus-core.workspace = true\nserde = \"1\"\n[dev-dependencies]\nmuxus-ssh = { workspace = true }\n[target.'cfg(unix)'.dependencies]\nmuxus-local.workspace = true\n";
        assert_eq!(
            internal_dependencies(manifest),
            vec!["muxus-core", "muxus-local"]
        );
    }
}
