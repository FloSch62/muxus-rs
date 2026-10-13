//! Terminal color schemes the user created or imported (port of the pure
//! parts of `client/src/terminal/custom-schemes.ts`). Turning a scheme into a
//! terminal theme belongs to the terminal view; this module keeps the stored
//! shape, its validation, the scheme files and the color arithmetic.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use muxus_core::js;

use super::command_buttons::{bounded_string, new_preference_id};
use super::highlight_profiles::js_number_text;

pub const TERMINAL_SCHEME_FILE_FORMAT: &str = "muxus-terminal-color-schemes";
pub const TERMINAL_SCHEME_FILE_VERSION: u32 = 1;
pub const MAX_TERMINAL_SCHEME_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_CUSTOM_TERMINAL_SCHEMES: usize = 100;
/// Host metadata stores its scheme override in at most 64 characters.
const MAX_SCHEME_ID_LENGTH: usize = 64;
pub const MAX_TERMINAL_SCHEME_NAME_LENGTH: usize = 100;
/// Names taken from a file or another scheme leave room for " copy" and a number.
const MAX_DERIVED_NAME_LENGTH: usize = 80;

/// The built-in schemes (`TERMINAL_SCHEMES` in `client/src/terminal/palette.ts`):
/// id, name and whether it is a light scheme. A custom scheme may not take a
/// built-in id, and copies avoid built-in names.
pub const BUILTIN_TERMINAL_SCHEMES: [(&str, &str, bool); 15] = [
    ("paper", "Paper", true),
    ("vscode-light", "Light (VS Code)", true),
    ("github-light", "GitHub Light", true),
    ("gruvbox-light", "Gruvbox Light", true),
    ("catppuccin-latte", "Catppuccin Latte", true),
    ("solarized-light", "Solarized Light", true),
    ("vscode-dark", "Dark (VS Code)", false),
    ("muxus", "Muxus", false),
    ("dracula", "Dracula", false),
    ("one-dark", "One Dark", false),
    ("nord", "Nord", false),
    ("gruvbox-dark", "Gruvbox Dark", false),
    ("catppuccin-mocha", "Catppuccin Mocha", false),
    ("monokai", "Monokai", false),
    ("solarized-dark", "Solarized Dark", false),
];

pub const SCHEME_BASE_COLOR_KEYS: [&str; 4] =
    ["background", "foreground", "cursor", "selectionBackground"];
pub const SCHEME_ANSI_COLOR_KEYS: [&str; 8] = [
    "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
];
pub const SCHEME_BRIGHT_ANSI_COLOR_KEYS: [&str; 8] = [
    "brightBlack",
    "brightRed",
    "brightGreen",
    "brightYellow",
    "brightBlue",
    "brightMagenta",
    "brightCyan",
    "brightWhite",
];

/// Every color key, in stored order.
pub fn scheme_color_keys() -> impl Iterator<Item = &'static str> {
    SCHEME_BASE_COLOR_KEYS
        .into_iter()
        .chain(SCHEME_ANSI_COLOR_KEYS)
        .chain(SCHEME_BRIGHT_ANSI_COLOR_KEYS)
}

/// Every color is an opaque `#rrggbb`, which is what a color well edits.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct CustomSchemeColors {
    pub background: String,
    pub foreground: String,
    pub cursor: String,
    pub selection_background: String,
    pub black: String,
    pub red: String,
    pub green: String,
    pub yellow: String,
    pub blue: String,
    pub magenta: String,
    pub cyan: String,
    pub white: String,
    pub bright_black: String,
    pub bright_red: String,
    pub bright_green: String,
    pub bright_yellow: String,
    pub bright_blue: String,
    pub bright_magenta: String,
    pub bright_cyan: String,
    pub bright_white: String,
}

impl CustomSchemeColors {
    /// The color under a stored key (`background`, `brightRed`, …).
    pub fn get(&self, key: &str) -> Option<&str> {
        Some(match key {
            "background" => &self.background,
            "foreground" => &self.foreground,
            "cursor" => &self.cursor,
            "selectionBackground" => &self.selection_background,
            "black" => &self.black,
            "red" => &self.red,
            "green" => &self.green,
            "yellow" => &self.yellow,
            "blue" => &self.blue,
            "magenta" => &self.magenta,
            "cyan" => &self.cyan,
            "white" => &self.white,
            "brightBlack" => &self.bright_black,
            "brightRed" => &self.bright_red,
            "brightGreen" => &self.bright_green,
            "brightYellow" => &self.bright_yellow,
            "brightBlue" => &self.bright_blue,
            "brightMagenta" => &self.bright_magenta,
            "brightCyan" => &self.bright_cyan,
            "brightWhite" => &self.bright_white,
            _ => return None,
        })
    }

    fn from_keys(mut color: impl FnMut(&str) -> String) -> Self {
        Self {
            background: color("background"),
            foreground: color("foreground"),
            cursor: color("cursor"),
            selection_background: color("selectionBackground"),
            black: color("black"),
            red: color("red"),
            green: color("green"),
            yellow: color("yellow"),
            blue: color("blue"),
            magenta: color("magenta"),
            cyan: color("cyan"),
            white: color("white"),
            bright_black: color("brightBlack"),
            bright_red: color("brightRed"),
            bright_green: color("brightGreen"),
            bright_yellow: color("brightYellow"),
            bright_blue: color("brightBlue"),
            bright_magenta: color("brightMagenta"),
            bright_cyan: color("brightCyan"),
            bright_white: color("brightWhite"),
        }
    }
}

/// A terminal color scheme the user created or imported, kept in preferences.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct CustomTerminalScheme {
    pub id: String,
    pub name: String,
    pub colors: CustomSchemeColors,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSchemeDocument {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub schemes: Vec<CustomTerminalScheme>,
}

/// Whether an id names a built-in scheme.
pub fn is_builtin_terminal_scheme_id(id: &str) -> bool {
    BUILTIN_TERMINAL_SCHEMES
        .iter()
        .any(|(builtin, _, _)| *builtin == id)
}

/// `isCustomTerminalSchemeArray`.
pub fn is_custom_terminal_scheme_array(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    if entries.len() > MAX_CUSTOM_TERMINAL_SCHEMES {
        return false;
    }
    let mut ids = HashSet::new();
    entries.iter().all(|entry| {
        let Some(record) = entry.as_object() else {
            return false;
        };
        let Some(id) = bounded_string(
            record.get("id").unwrap_or(&Value::Null),
            MAX_SCHEME_ID_LENGTH,
        ) else {
            return false;
        };
        let Some(name) = bounded_string(
            record.get("name").unwrap_or(&Value::Null),
            MAX_TERMINAL_SCHEME_NAME_LENGTH,
        ) else {
            return false;
        };
        // A built-in id would be shadowed by the built-in scheme.
        if is_builtin_terminal_scheme_id(id)
            || ids.contains(id)
            || js::js_trim(name).is_empty()
            || !is_scheme_colors(record.get("colors").unwrap_or(&Value::Null))
        {
            return false;
        }
        ids.insert(id.to_owned());
        true
    })
}

/// [`is_custom_terminal_scheme_array`] for typed schemes.
pub fn are_valid_custom_schemes(schemes: &[CustomTerminalScheme]) -> bool {
    serde_json::to_value(schemes).is_ok_and(|value| is_custom_terminal_scheme_array(&value))
}

fn is_scheme_colors(value: &Value) -> bool {
    let Some(colors) = value.as_object() else {
        return false;
    };
    colors.len() == scheme_color_keys().count()
        && scheme_color_keys()
            .all(|key| colors.get(key).and_then(Value::as_str).is_some_and(is_hex6))
}

/// `/^#[0-9a-f]{6}$/i`.
fn is_hex6(text: &str) -> bool {
    super::highlight_profiles::is_hex_color(text)
}

/// Whether black text reads better than white on this `#rrggbb` background.
pub fn is_light_color(color: &str) -> bool {
    let channel = |offset: usize| {
        let value = color
            .get(offset..offset + 2)
            .and_then(|hex| u8::from_str_radix(hex, 16).ok())
            .unwrap_or(0);
        let linear = f64::from(value) / 255.0;
        if linear <= 0.04045 {
            linear / 12.92
        } else {
            ((linear + 0.055) / 1.055).powf(2.4)
        }
    };
    // The WCAG relative luminance at which both contrast ratios are equal.
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5) > 0.179
}

/// A name not yet used by a built-in or an existing custom scheme.
pub fn unused_scheme_name(name: &str, existing: &[CustomTerminalScheme]) -> String {
    let taken: HashSet<String> = BUILTIN_TERMINAL_SCHEMES
        .iter()
        .map(|(_, builtin, _)| builtin.to_lowercase())
        .chain(existing.iter().map(|scheme| scheme.name.to_lowercase()))
        .collect();
    if !taken.contains(&name.to_lowercase()) {
        return name.to_owned();
    }
    let mut suffix = 2;
    while taken.contains(&format!("{name} {suffix}").to_lowercase()) {
        suffix += 1;
    }
    format!("{name} {suffix}")
}

fn derived_scheme_name(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let name = js::js_trim(js::utf16_prefix(js::js_trim(text), MAX_DERIVED_NAME_LENGTH));
    (!name.is_empty()).then(|| name.to_owned())
}

/// The file name without its last extension, as a scheme name.
pub fn scheme_name_from_filename(filename: &str) -> String {
    let stem = match filename.rfind('.') {
        Some(dot) => &filename[..dot],
        None => filename,
    };
    derived_scheme_name(Some(&Value::String(stem.to_owned())))
        .unwrap_or_else(|| "Imported scheme".to_owned())
}

/// Strip unknown fields and lowercase the colors (`copyScheme`).
pub fn copy_scheme(scheme: &CustomTerminalScheme) -> CustomTerminalScheme {
    CustomTerminalScheme {
        id: scheme.id.clone(),
        name: scheme.name.clone(),
        colors: CustomSchemeColors::from_keys(|key| {
            scheme.colors.get(key).unwrap_or_default().to_lowercase()
        }),
    }
}

/// A new custom scheme that starts out as a copy of another one. `theme`
/// holds the source's colors in any notation a theme uses (translucent
/// selections included), keyed like [`CustomSchemeColors`].
pub fn custom_scheme_copy_of(
    name: &str,
    theme: &Map<String, Value>,
    existing: &[CustomTerminalScheme],
) -> Result<CustomTerminalScheme, String> {
    let copy_name = format!("{} copy", js::utf16_prefix(name, MAX_DERIVED_NAME_LENGTH));
    Ok(CustomTerminalScheme {
        id: new_preference_id("scheme"),
        name: unused_scheme_name(&copy_name, existing),
        colors: opaque_scheme_colors(theme, name)?,
    })
}

/// Build the portable file shared by the custom scheme import and export.
pub fn create_terminal_scheme_document(
    schemes: &[CustomTerminalScheme],
) -> Result<TerminalSchemeDocument, String> {
    if schemes.is_empty() || !are_valid_custom_schemes(schemes) {
        return Err("Select at least one valid color scheme to export.".into());
    }
    Ok(TerminalSchemeDocument {
        format: TERMINAL_SCHEME_FILE_FORMAT.into(),
        version: TERMINAL_SCHEME_FILE_VERSION,
        created_at: js::now_iso(),
        schemes: schemes.iter().map(copy_scheme).collect(),
    })
}

/// Read the schemes in an untrusted file: a Muxus scheme export, a Windows
/// Terminal color scheme (one object, a list, or the "schemes" of a settings
/// file), or an iTerm2 `.itermcolors` preset. Only a Muxus export carries
/// ids; schemes from the other formats get new ones and are named after the
/// file when they do not name themselves.
pub fn parse_terminal_scheme_file(
    text: &str,
    filename: &str,
) -> Result<Vec<CustomTerminalScheme>, String> {
    let file_scheme_name = scheme_name_from_filename(filename);
    if js::js_trim_start(text).starts_with('<') {
        return Ok(vec![parse_iterm_colors(text, &file_scheme_name)?]);
    }
    let parsed: Value = serde_json::from_str(text).map_err(|_| {
        "This file is neither a JSON color scheme nor an iTerm2 color preset.".to_owned()
    })?;
    if let Some(record) = parsed.as_object()
        && record.get("format").and_then(Value::as_str) == Some(TERMINAL_SCHEME_FILE_FORMAT)
    {
        return parse_scheme_document(record);
    }
    let entries: Vec<Value> = match &parsed {
        Value::Array(entries) => entries.clone(),
        Value::Object(record) if record.get("schemes").is_some_and(Value::is_array) => {
            record["schemes"].as_array().cloned().unwrap_or_default()
        }
        other => vec![other.clone()],
    };
    if entries.is_empty() {
        return Err("This file contains no color schemes.".into());
    }
    if entries.len() > MAX_CUSTOM_TERMINAL_SCHEMES {
        return Err(format!(
            "This file contains more than {MAX_CUSTOM_TERMINAL_SCHEMES} color schemes."
        ));
    }
    let count = entries.len();
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let fallback = if count == 1 {
                file_scheme_name.clone()
            } else {
                format!("{file_scheme_name} {}", index + 1)
            };
            parse_json_scheme(entry, &fallback)
        })
        .collect()
}

/// Add imported schemes to the user's own. A scheme whose id is already
/// known is replaced in place, so terminal themes and hosts that use it keep
/// following it; every other scheme is added under a name that is still free.
pub fn merge_custom_terminal_schemes(
    current: &[CustomTerminalScheme],
    imported: &[CustomTerminalScheme],
) -> Result<Vec<CustomTerminalScheme>, String> {
    let mut merged = current.to_vec();
    for scheme in imported {
        if let Some(index) = merged
            .iter()
            .position(|candidate| candidate.id == scheme.id)
        {
            merged[index] = scheme.clone();
        } else {
            let name = unused_scheme_name(&scheme.name, &merged);
            merged.push(CustomTerminalScheme {
                name,
                ..scheme.clone()
            });
        }
    }
    if merged.len() > MAX_CUSTOM_TERMINAL_SCHEMES {
        return Err(format!(
            "This import would exceed the limit of {MAX_CUSTOM_TERMINAL_SCHEMES} custom color schemes. Delete an existing scheme and try again."
        ));
    }
    Ok(merged)
}

fn parse_scheme_document(
    document: &Map<String, Value>,
) -> Result<Vec<CustomTerminalScheme>, String> {
    match document.get("version").and_then(Value::as_f64) {
        Some(version) if version == f64::from(TERMINAL_SCHEME_FILE_VERSION) => {}
        Some(version) => {
            return Err(format!(
                "Color scheme file version {} is not supported.",
                js_number_text(version)
            ));
        }
        None => return Err("This file is missing a supported color scheme version.".into()),
    }
    let schemes = document.get("schemes").unwrap_or(&Value::Null);
    if !is_custom_terminal_scheme_array(schemes) || schemes.as_array().is_none_or(Vec::is_empty) {
        return Err("The color scheme file is incomplete or invalid.".into());
    }
    let schemes: Vec<CustomTerminalScheme> = serde_json::from_value(schemes.clone())
        .map_err(|_| "The color scheme file is incomplete or invalid.".to_owned())?;
    Ok(schemes.iter().map(copy_scheme).collect())
}

fn parse_json_scheme(entry: &Value, fallback_name: &str) -> Result<CustomTerminalScheme, String> {
    let Some(record) = entry.as_object() else {
        return Err("This file does not describe a terminal color scheme.".into());
    };
    let name = derived_scheme_name(record.get("name")).unwrap_or_else(|| fallback_name.to_owned());
    let colors = record
        .get("colors")
        .and_then(Value::as_object)
        .unwrap_or(record);
    let mut source = colors.clone();
    // Windows Terminal says purple and cursorColor where xterm says magenta and cursor.
    let either = |primary: &str, alternate: &str| {
        colors
            .get(primary)
            .filter(|value| !value.is_null())
            .or_else(|| colors.get(alternate))
            .cloned()
    };
    for (key, primary, alternate) in [
        ("magenta", "magenta", "purple"),
        ("brightMagenta", "brightMagenta", "brightPurple"),
        ("cursor", "cursor", "cursorColor"),
    ] {
        source.insert(
            key.into(),
            either(primary, alternate).unwrap_or(Value::Null),
        );
    }
    Ok(CustomTerminalScheme {
        id: new_preference_id("scheme"),
        name: name.clone(),
        colors: opaque_scheme_colors(&source, &name)?,
    })
}

fn iterm_color_name(key: &str) -> &'static str {
    match key {
        "background" => "Background Color",
        "foreground" => "Foreground Color",
        "cursor" => "Cursor Color",
        "selectionBackground" => "Selection Color",
        "black" => "Ansi 0 Color",
        "red" => "Ansi 1 Color",
        "green" => "Ansi 2 Color",
        "yellow" => "Ansi 3 Color",
        "blue" => "Ansi 4 Color",
        "magenta" => "Ansi 5 Color",
        "cyan" => "Ansi 6 Color",
        "white" => "Ansi 7 Color",
        "brightBlack" => "Ansi 8 Color",
        "brightRed" => "Ansi 9 Color",
        "brightGreen" => "Ansi 10 Color",
        "brightYellow" => "Ansi 11 Color",
        "brightBlue" => "Ansi 12 Color",
        "brightMagenta" => "Ansi 13 Color",
        "brightCyan" => "Ansi 14 Color",
        _ => "Ansi 15 Color",
    }
}

fn skip_whitespace(text: &str, at: usize) -> usize {
    at + (text[at..].len() - text[at..].trim_start_matches(char::is_whitespace).len())
}

/// Every `<key>NAME</key>\s*<dict>BODY</dict>` (shortest body) in order.
fn iterm_color_dicts(text: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = text[from..].find("<key>") {
        let start = from + offset;
        let name_start = start + "<key>".len();
        let matched = (|| {
            let name_end = name_start + text[name_start..].find('<')?;
            if name_end == name_start || !text[name_end..].starts_with("</key>") {
                return None;
            }
            let dict_open = skip_whitespace(text, name_end + "</key>".len());
            if !text[dict_open..].starts_with("<dict>") {
                return None;
            }
            let body_start = dict_open + "<dict>".len();
            let body_end = body_start + text[body_start..].find("</dict>")?;
            Some((
                &text[name_start..name_end],
                &text[body_start..body_end],
                body_end + "</dict>".len(),
            ))
        })();
        match matched {
            Some((name, body, end)) => {
                found.push((name, body));
                from = end;
            }
            None => from = start + 1,
        }
    }
    found
}

/// Every `<key>(Red|Green|Blue|Alpha) Component</key>\s*<(real|integer)>VALUE<`.
fn iterm_components(body: &str) -> Vec<(&str, &str)> {
    let mut found = Vec::new();
    let mut from = 0;
    while let Some(offset) = body[from..].find("<key>") {
        let start = from + offset;
        let name_start = start + "<key>".len();
        let matched = (|| {
            let component = ["Red", "Green", "Blue", "Alpha"]
                .into_iter()
                .find(|component| {
                    body[name_start..].starts_with(&format!("{component} Component</key>"))
                })?;
            let after = name_start + component.len() + " Component</key>".len();
            let open = skip_whitespace(body, after);
            let value_start = ["<real>", "<integer>"]
                .into_iter()
                .find(|tag| body[open..].starts_with(tag))
                .map(|tag| open + tag.len())?;
            let value_end = value_start + body[value_start..].find('<')?;
            Some((component, &body[value_start..value_end], value_end + 1))
        })();
        match matched {
            Some((component, value, end)) => {
                found.push((component, value));
                from = end;
            }
            None => from = start + 1,
        }
    }
    found
}

/// `Number(text)` for the plain decimal numbers a property list holds.
fn js_number(text: &str) -> f64 {
    let trimmed = js::js_trim(text);
    if trimmed.is_empty() {
        0.0
    } else {
        trimmed.parse().unwrap_or(f64::NAN)
    }
}

/// `Math.round` for non-negative values.
fn js_round(value: f64) -> f64 {
    (value + 0.5).floor()
}

/// An iTerm2 preset is a property list of named colors, each a flat
/// dictionary of 0–1 components. The export is machine-written and this
/// regular, so the colors are picked out directly; the untrusted file never
/// reaches an XML parser.
fn parse_iterm_colors(text: &str, name: &str) -> Result<CustomTerminalScheme, String> {
    let has_plist = text.match_indices("<plist").any(|(index, _)| {
        text[index + "<plist".len()..]
            .chars()
            .next()
            .is_some_and(|c| c == '>' || c.is_whitespace())
    });
    if !has_plist {
        return Err("This file is neither a JSON color scheme nor an iTerm2 color preset.".into());
    }
    let mut colors_by_name: Vec<(String, String)> = Vec::new();
    for (color_name, dictionary) in iterm_color_dicts(text) {
        let mut components: Vec<(&str, f64)> = Vec::new();
        for (component, value) in iterm_components(dictionary) {
            components.retain(|(known, _)| *known != component);
            components.push((component, js_number(value)));
        }
        let get = |component: &str| {
            components
                .iter()
                .find(|(known, _)| *known == component)
                .map(|(_, value)| *value)
        };
        let channels: Vec<Option<f64>> = ["Red", "Green", "Blue"].into_iter().map(get).collect();
        if channels
            .iter()
            .any(|channel| !channel.is_some_and(f64::is_finite))
        {
            continue;
        }
        let alpha = get("Alpha").unwrap_or(1.0);
        let alpha = if alpha.is_finite() { alpha } else { 1.0 };
        let hex: String = channels
            .into_iter()
            .map(|channel| channel.unwrap_or(0.0))
            .chain([alpha])
            .map(|channel| format!("{:02x}", js_round(channel.clamp(0.0, 1.0) * 255.0) as u8))
            .collect();
        let color_name = js::js_trim(color_name).to_owned();
        colors_by_name.retain(|(known, _)| *known != color_name);
        colors_by_name.push((color_name, format!("#{hex}")));
    }
    let mut source = Map::new();
    for key in scheme_color_keys() {
        let wanted = iterm_color_name(key);
        let color = colors_by_name
            .iter()
            .find(|(known, _)| known == wanted)
            .map(|(_, color)| color.clone());
        source.insert(key.into(), color.map_or(Value::Null, Value::String));
    }
    Ok(CustomTerminalScheme {
        id: new_preference_id("scheme"),
        name: name.to_owned(),
        colors: opaque_scheme_colors(&source, name)?,
    })
}

fn scheme_color_label(key: &str) -> &str {
    match key {
        "foreground" => "foreground",
        "background" => "background",
        "magenta" => "magenta (or purple)",
        "brightMagenta" => "brightMagenta (or brightPurple)",
        other => other,
    }
}

/// A color with straight alpha, channels 0–255.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rgba {
    pub r: f64,
    pub g: f64,
    pub b: f64,
    pub a: f64,
}

/// Normalize a full palette, in any notation a scheme file or a built-in
/// theme uses, to opaque `#rrggbb`. A translucent color is flattened over the
/// background — the built-in selections are translucent, and that is the
/// color the terminal paints. The cursor and the selection are optional in
/// most formats and derive from the text color when a scheme leaves them out.
pub fn opaque_scheme_colors(
    source: &Map<String, Value>,
    scheme_name: &str,
) -> Result<CustomSchemeColors, String> {
    let required = |key: &str| -> Result<Rgba, String> {
        parse_color(source.get(key)).ok_or_else(|| {
            format!(
                "{scheme_name}: the color \"{}\" is missing or is not a color such as \"#1e1e2e\".",
                scheme_color_label(key)
            )
        })
    };
    let background = Rgba {
        a: 1.0,
        ..required("background")?
    };
    let foreground = flatten(required("foreground")?, background);
    let cursor = flatten(
        parse_color(source.get("cursor")).unwrap_or(foreground),
        background,
    );
    let selection = flatten(
        parse_color(source.get("selectionBackground")).unwrap_or(Rgba {
            a: 0.3,
            ..foreground
        }),
        background,
    );
    let mut ansi = Map::new();
    for key in SCHEME_ANSI_COLOR_KEYS
        .into_iter()
        .chain(SCHEME_BRIGHT_ANSI_COLOR_KEYS)
    {
        ansi.insert(
            key.into(),
            Value::String(to_hex(flatten(required(key)?, background))),
        );
    }
    Ok(CustomSchemeColors::from_keys(|key| match key {
        "background" => to_hex(background),
        "foreground" => to_hex(foreground),
        "cursor" => to_hex(cursor),
        "selectionBackground" => to_hex(selection),
        other => ansi[other].as_str().unwrap_or_default().to_owned(),
    }))
}

/// `#rgb`, `#rrggbb`, `#rrggbbaa`, `rgb(r, g, b)` or `rgba(r, g, b, a)`.
pub fn parse_color(value: Option<&Value>) -> Option<Rgba> {
    let text = js::js_trim(value?.as_str()?);
    if let Some(hex) = text.strip_prefix('#')
        && matches!(hex.len(), 3 | 6 | 8)
        && hex.bytes().all(|b| b.is_ascii_hexdigit())
    {
        let digits: String = if hex.len() == 3 {
            hex.chars().flat_map(|c| [c, c]).collect()
        } else {
            hex.to_owned()
        };
        let channel = |offset: usize| {
            f64::from(u8::from_str_radix(&digits[offset..offset + 2], 16).unwrap_or(0))
        };
        return Some(Rgba {
            r: channel(0),
            g: channel(2),
            b: channel(4),
            a: if digits.len() == 8 {
                channel(6) / 255.0
            } else {
                1.0
            },
        });
    }
    parse_rgb_notation(text)
}

/// `/^rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*(?:,\s*(\d*\.?\d+)\s*)?\)$/i`.
fn parse_rgb_notation(text: &str) -> Option<Rgba> {
    let lower = text.to_ascii_lowercase();
    let rest = lower
        .strip_prefix("rgba(")
        .or_else(|| lower.strip_prefix("rgb("))?;
    let body = rest.strip_suffix(')')?;
    let parts: Vec<&str> = body
        .split(',')
        .map(|part| part.trim_matches(char::is_whitespace))
        .collect();
    if !(3..=4).contains(&parts.len()) {
        return None;
    }
    let channel = |part: &str| -> Option<f64> {
        (!part.is_empty() && part.len() <= 3 && part.bytes().all(|b| b.is_ascii_digit()))
            .then(|| part.parse().ok())?
    };
    let r = channel(parts[0])?;
    let g = channel(parts[1])?;
    let b = channel(parts[2])?;
    if r > 255.0 || g > 255.0 || b > 255.0 {
        return None;
    }
    let a = match parts.get(3) {
        None => 1.0,
        Some(alpha) => {
            // \d*\.?\d+
            let (whole, fraction) = alpha.split_once('.').unwrap_or(("", alpha));
            let valid = !fraction.is_empty()
                && whole.bytes().all(|b| b.is_ascii_digit())
                && fraction.bytes().all(|b| b.is_ascii_digit());
            if !valid {
                return None;
            }
            alpha.parse::<f64>().ok()?.min(1.0)
        }
    };
    Some(Rgba { r, g, b, a })
}

/// Paint `color` over an opaque backdrop.
pub fn flatten(color: Rgba, backdrop: Rgba) -> Rgba {
    if color.a >= 1.0 {
        return color;
    }
    let mix = |top: f64, bottom: f64| js_round(top * color.a + bottom * (1.0 - color.a));
    Rgba {
        r: mix(color.r, backdrop.r),
        g: mix(color.g, backdrop.g),
        b: mix(color.b, backdrop.b),
        a: 1.0,
    }
}

/// `#rrggbb`.
pub fn to_hex(color: Rgba) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        color.r as u8, color.g as u8, color.b as u8
    )
}
