//! Color schemes the user creates or imports (port of `client/src/terminal/custom-schemes.ts`):
//! validation of stored schemes, the portable export file, and importers for Muxus exports,
//! Windows Terminal schemes and iTerm2 `.itermcolors` presets.

use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::palette::{
    TerminalScheme, TerminalTheme, terminal_schemes, without_overview_ruler_border,
};

pub const TERMINAL_SCHEME_FILE_FORMAT: &str = "muxus-terminal-color-schemes";
pub const TERMINAL_SCHEME_FILE_VERSION: u32 = 1;
pub const MAX_TERMINAL_SCHEME_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_CUSTOM_TERMINAL_SCHEMES: usize = 100;
/// Host metadata stores its scheme override in at most 64 characters.
const MAX_SCHEME_ID_LENGTH: usize = 64;
pub const MAX_TERMINAL_SCHEME_NAME_LENGTH: usize = 100;
/// Names taken from a file or another scheme leave room for " copy" and a number.
const MAX_DERIVED_NAME_LENGTH: usize = 80;

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

/// Every key a custom scheme stores, in file order.
pub const SCHEME_COLOR_KEYS: [&str; 20] = [
    "background",
    "foreground",
    "cursor",
    "selectionBackground",
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

/// Every color is an opaque `#rrggbb`, which is what a color well edits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
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
    /// A color by its file key (`background`, `brightWhite`, …).
    pub fn get(&self, key: &str) -> Option<&str> {
        self.slot(key).map(|value| value.as_str())
    }

    fn slot(&self, key: &str) -> Option<&String> {
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

    fn from_fn(mut color: impl FnMut(&str) -> String) -> Self {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CustomTerminalScheme {
    pub id: String,
    pub name: String,
    pub colors: CustomSchemeColors,
}

/// The portable file shared by the custom scheme import and export.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSchemeDocument {
    pub format: String,
    pub version: u32,
    pub created_at: String,
    pub schemes: Vec<CustomTerminalScheme>,
}

/// A scheme file or scheme list that cannot be used; the message is shown to the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemeError(pub String);

impl fmt::Display for SchemeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for SchemeError {}

fn error<T>(message: impl Into<String>) -> Result<T, SchemeError> {
    Err(SchemeError(message.into()))
}

fn is_hex6(value: &str) -> bool {
    crate::palette::is_hex_color(value)
}

fn bounded_string(value: &Value, max: usize) -> Option<&str> {
    let text = value.as_str()?;
    let length = text.encode_utf16().count();
    (length > 0 && length <= max).then_some(text)
}

/// Whether untrusted JSON is a valid list of custom schemes, as stored in preferences.
pub fn is_custom_terminal_scheme_array(value: &Value) -> bool {
    let Some(entries) = value.as_array() else {
        return false;
    };
    if entries.len() > MAX_CUSTOM_TERMINAL_SCHEMES {
        return false;
    }
    let mut ids = std::collections::HashSet::new();
    entries.iter().all(|entry| {
        let Some(record) = entry.as_object() else {
            return false;
        };
        let Some(id) = record
            .get("id")
            .and_then(|id| bounded_string(id, MAX_SCHEME_ID_LENGTH))
        else {
            return false;
        };
        // A built-in id would be shadowed by the built-in scheme.
        if terminal_schemes().iter().any(|scheme| scheme.id == id) || ids.contains(id) {
            return false;
        }
        let Some(name) = record
            .get("name")
            .and_then(|name| bounded_string(name, MAX_TERMINAL_SCHEME_NAME_LENGTH))
        else {
            return false;
        };
        if name.trim().is_empty() || !record.get("colors").is_some_and(is_scheme_colors) {
            return false;
        }
        ids.insert(id.to_string());
        true
    })
}

/// The typed form of [`is_custom_terminal_scheme_array`].
pub fn custom_terminal_schemes_valid(schemes: &[CustomTerminalScheme]) -> bool {
    serde_json::to_value(schemes).is_ok_and(|value| is_custom_terminal_scheme_array(&value))
}

fn is_scheme_colors(value: &Value) -> bool {
    let Some(record) = value.as_object() else {
        return false;
    };
    record.len() == SCHEME_COLOR_KEYS.len()
        && SCHEME_COLOR_KEYS.iter().all(|key| {
            record
                .get(*key)
                .and_then(Value::as_str)
                .is_some_and(is_hex6)
        })
}

/// A custom scheme in the shape the terminal and the scheme pickers consume.
pub fn terminal_scheme_from_custom(custom: &CustomTerminalScheme) -> TerminalScheme {
    let colors = &custom.colors;
    TerminalScheme {
        id: custom.id.clone(),
        name: custom.name.clone(),
        light: is_light_color(&colors.background),
        theme: without_overview_ruler_border(TerminalTheme {
            background: colors.background.clone(),
            foreground: colors.foreground.clone(),
            cursor: colors.cursor.clone(),
            cursor_accent: colors.background.clone(),
            selection_background: colors.selection_background.clone(),
            ansi: [
                colors.black.clone(),
                colors.red.clone(),
                colors.green.clone(),
                colors.yellow.clone(),
                colors.blue.clone(),
                colors.magenta.clone(),
                colors.cyan.clone(),
                colors.white.clone(),
                colors.bright_black.clone(),
                colors.bright_red.clone(),
                colors.bright_green.clone(),
                colors.bright_yellow.clone(),
                colors.bright_blue.clone(),
                colors.bright_magenta.clone(),
                colors.bright_cyan.clone(),
                colors.bright_white.clone(),
            ],
            overview_ruler_border: None,
        }),
    }
}

pub fn terminal_schemes_from_custom(
    custom_schemes: &[CustomTerminalScheme],
) -> Vec<TerminalScheme> {
    custom_schemes
        .iter()
        .map(terminal_scheme_from_custom)
        .collect()
}

/// A fresh preference id, as `newPreferenceId` makes them: `<prefix>-<uuid>`.
pub fn new_preference_id(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// A new custom scheme that starts out as a copy of a built-in or custom one.
pub fn custom_scheme_copy_of(
    scheme: &TerminalScheme,
    existing: &[CustomTerminalScheme],
) -> Result<CustomTerminalScheme, SchemeError> {
    let base: String = scheme.name.chars().take(MAX_DERIVED_NAME_LENGTH).collect();
    let theme = &scheme.theme;
    Ok(CustomTerminalScheme {
        id: new_preference_id("scheme"),
        name: unused_scheme_name(&format!("{base} copy"), existing),
        colors: opaque_scheme_colors(|key| theme.color(key).map(Value::from), &scheme.name)?,
    })
}

/// Build the portable file shared by the custom scheme import and export.
pub fn create_terminal_scheme_document(
    schemes: &[CustomTerminalScheme],
) -> Result<TerminalSchemeDocument, SchemeError> {
    if schemes.is_empty() || !custom_terminal_schemes_valid(schemes) {
        return error("Select at least one valid color scheme to export.");
    }
    Ok(TerminalSchemeDocument {
        format: TERMINAL_SCHEME_FILE_FORMAT.into(),
        version: TERMINAL_SCHEME_FILE_VERSION,
        created_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        schemes: schemes.iter().map(copy_scheme).collect(),
    })
}

/// Read the schemes in an untrusted file: a Muxus scheme export, a Windows Terminal color scheme
/// (one object, a list, or the "schemes" of a settings file), or an iTerm2 `.itermcolors` preset.
/// Only a Muxus export carries ids; schemes from the other formats get new ones and are named
/// after the file when they do not name themselves.
pub fn parse_terminal_scheme_file(
    text: &str,
    filename: &str,
) -> Result<Vec<CustomTerminalScheme>, SchemeError> {
    let file_scheme_name = scheme_name_from_filename(filename);
    if text.trim_start().starts_with('<') {
        return Ok(vec![parse_iterm_colors(text, &file_scheme_name)?]);
    }
    let Ok(parsed) = serde_json::from_str::<Value>(text) else {
        return error("This file is neither a JSON color scheme nor an iTerm2 color preset.");
    };
    if let Some(document) = parsed.as_object()
        && document.get("format").and_then(Value::as_str) == Some(TERMINAL_SCHEME_FILE_FORMAT)
    {
        return parse_scheme_document(document);
    }
    let entries: Vec<Value> = match &parsed {
        Value::Array(list) => list.clone(),
        Value::Object(record) if record.get("schemes").is_some_and(Value::is_array) => {
            record["schemes"].as_array().cloned().unwrap_or_default()
        }
        other => vec![other.clone()],
    };
    if entries.is_empty() {
        return error("This file contains no color schemes.");
    }
    if entries.len() > MAX_CUSTOM_TERMINAL_SCHEMES {
        return error(format!(
            "This file contains more than {MAX_CUSTOM_TERMINAL_SCHEMES} color schemes."
        ));
    }
    let single = entries.len() == 1;
    entries
        .iter()
        .enumerate()
        .map(|(index, entry)| {
            let fallback = if single {
                file_scheme_name.clone()
            } else {
                format!("{file_scheme_name} {}", index + 1)
            };
            parse_json_scheme(entry, &fallback)
        })
        .collect()
}

/// Add imported schemes to the user's own. A scheme whose id is already known is replaced in
/// place, so terminal themes and hosts that use it keep following it; every other scheme is added
/// under a name that is still free.
pub fn merge_custom_terminal_schemes(
    current: &[CustomTerminalScheme],
    imported: &[CustomTerminalScheme],
) -> Result<Vec<CustomTerminalScheme>, SchemeError> {
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
        return error(format!(
            "This import would exceed the limit of {MAX_CUSTOM_TERMINAL_SCHEMES} custom color schemes. Delete an existing scheme and try again."
        ));
    }
    Ok(merged)
}

fn parse_scheme_document(
    document: &Map<String, Value>,
) -> Result<Vec<CustomTerminalScheme>, SchemeError> {
    let version = document.get("version");
    if version.and_then(Value::as_f64) != Some(f64::from(TERMINAL_SCHEME_FILE_VERSION)) {
        return match version.filter(|v| v.is_number()) {
            Some(number) => error(format!(
                "Color scheme file version {number} is not supported."
            )),
            None => error("This file is missing a supported color scheme version."),
        };
    }
    let schemes = document.get("schemes").cloned().unwrap_or(Value::Null);
    if !is_custom_terminal_scheme_array(&schemes) || schemes.as_array().is_none_or(Vec::is_empty) {
        return error("The color scheme file is incomplete or invalid.");
    }
    let schemes: Vec<CustomTerminalScheme> = serde_json::from_value(schemes)
        .map_err(|_| SchemeError("The color scheme file is incomplete or invalid.".into()))?;
    Ok(schemes.iter().map(copy_scheme).collect())
}

fn non_null<'a>(record: &'a Map<String, Value>, key: &str) -> Option<&'a Value> {
    record.get(key).filter(|value| !value.is_null())
}

fn parse_json_scheme(
    entry: &Value,
    fallback_name: &str,
) -> Result<CustomTerminalScheme, SchemeError> {
    let Some(record) = entry.as_object() else {
        return error("This file does not describe a terminal color scheme.");
    };
    let name = derived_scheme_name(record.get("name")).unwrap_or_else(|| fallback_name.to_string());
    let colors = record
        .get("colors")
        .and_then(Value::as_object)
        .unwrap_or(record);
    let source = |key: &str| -> Option<Value> {
        // Windows Terminal says purple and cursorColor where xterm says magenta and cursor.
        let value = match key {
            "magenta" => non_null(colors, "magenta").or_else(|| colors.get("purple")),
            "brightMagenta" => {
                non_null(colors, "brightMagenta").or_else(|| colors.get("brightPurple"))
            }
            "cursor" => non_null(colors, "cursor").or_else(|| colors.get("cursorColor")),
            _ => colors.get(key),
        };
        value.cloned()
    };
    Ok(CustomTerminalScheme {
        id: new_preference_id("scheme"),
        colors: opaque_scheme_colors(source, &name)?,
        name,
    })
}

const ITERM_COLOR_NAMES: [(&str, &str); 20] = [
    ("background", "Background Color"),
    ("foreground", "Foreground Color"),
    ("cursor", "Cursor Color"),
    ("selectionBackground", "Selection Color"),
    ("black", "Ansi 0 Color"),
    ("red", "Ansi 1 Color"),
    ("green", "Ansi 2 Color"),
    ("yellow", "Ansi 3 Color"),
    ("blue", "Ansi 4 Color"),
    ("magenta", "Ansi 5 Color"),
    ("cyan", "Ansi 6 Color"),
    ("white", "Ansi 7 Color"),
    ("brightBlack", "Ansi 8 Color"),
    ("brightRed", "Ansi 9 Color"),
    ("brightGreen", "Ansi 10 Color"),
    ("brightYellow", "Ansi 11 Color"),
    ("brightBlue", "Ansi 12 Color"),
    ("brightMagenta", "Ansi 13 Color"),
    ("brightCyan", "Ansi 14 Color"),
    ("brightWhite", "Ansi 15 Color"),
];

static ITERM_COLOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"<key>([^<]+)</key>\s*<dict>([\s\S]*?)</dict>").unwrap());
static ITERM_COMPONENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"<key>(Red|Green|Blue|Alpha) Component</key>\s*<(?:real|integer)>([^<]*)<").unwrap()
});
static PLIST_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"<plist[\s>]").unwrap());

/// JavaScript's `Number()` for the plain decimal text a property list holds.
fn js_number(text: &str) -> f64 {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return 0.0;
    }
    trimmed.parse().unwrap_or(f64::NAN)
}

/// An iTerm2 preset is a property list of named colors, each a flat dictionary of 0–1
/// components. The export is machine-written and this regular, so the colors are picked out
/// directly; the untrusted file, which always carries a DOCTYPE, never reaches an XML parser.
fn parse_iterm_colors(text: &str, name: &str) -> Result<CustomTerminalScheme, SchemeError> {
    if !PLIST_RE.is_match(text) {
        return error("This file is neither a JSON color scheme nor an iTerm2 color preset.");
    }
    let mut colors_by_name = std::collections::HashMap::new();
    for captures in ITERM_COLOR_RE.captures_iter(text) {
        let mut components = std::collections::HashMap::new();
        for component in ITERM_COMPONENT_RE.captures_iter(&captures[2]) {
            components.insert(component[1].to_string(), js_number(&component[2]));
        }
        let channels: Vec<Option<f64>> = ["Red", "Green", "Blue"]
            .iter()
            .map(|channel| {
                components
                    .get(*channel)
                    .copied()
                    .filter(|value| value.is_finite())
            })
            .collect();
        if channels.iter().any(Option::is_none) {
            continue;
        }
        let alpha = components.get("Alpha").copied().unwrap_or(1.0);
        let alpha = if alpha.is_finite() { alpha } else { 1.0 };
        let hex: String = channels
            .into_iter()
            .map(|channel| channel.unwrap_or(0.0))
            .chain([alpha])
            .map(|channel| format!("{:02x}", (channel.clamp(0.0, 1.0) * 255.0).round() as u8))
            .collect();
        colors_by_name.insert(captures[1].trim().to_string(), format!("#{hex}"));
    }
    let source = |key: &str| -> Option<Value> {
        let iterm = ITERM_COLOR_NAMES.iter().find(|(k, _)| *k == key)?.1;
        colors_by_name
            .get(iterm)
            .map(|color| Value::from(color.as_str()))
    };
    Ok(CustomTerminalScheme {
        id: new_preference_id("scheme"),
        name: name.to_string(),
        colors: opaque_scheme_colors(source, name)?,
    })
}

fn scheme_color_label(key: &str) -> &str {
    match key {
        "magenta" => "magenta (or purple)",
        "brightMagenta" => "brightMagenta (or brightPurple)",
        other => other,
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Color {
    r: f64,
    g: f64,
    b: f64,
    a: f64,
}

static HEX_NOTATION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^#([0-9a-f]{3}|[0-9a-f]{6}|[0-9a-f]{8})$").unwrap());
static RGB_NOTATION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^rgba?\(\s*(\d{1,3})\s*,\s*(\d{1,3})\s*,\s*(\d{1,3})\s*(?:,\s*(\d*\.?\d+)\s*)?\)$",
    )
    .unwrap()
});

fn parse_color(value: Option<&Value>) -> Option<Color> {
    let text = value?.as_str()?.trim();
    if let Some(captures) = HEX_NOTATION_RE.captures(text) {
        let hex = &captures[1];
        let digits: String = if hex.len() == 3 {
            hex.chars().flat_map(|c| [c, c]).collect()
        } else {
            hex.to_string()
        };
        let channel = |offset: usize| {
            f64::from(u8::from_str_radix(&digits[offset..offset + 2], 16).unwrap_or(0))
        };
        return Some(Color {
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
    let captures = RGB_NOTATION_RE.captures(text)?;
    let channel = |index: usize| captures[index].parse::<f64>().unwrap_or(f64::NAN);
    let (r, g, b) = (channel(1), channel(2), channel(3));
    if r > 255.0 || g > 255.0 || b > 255.0 {
        return None;
    }
    let a = captures.get(4).map_or(1.0, |alpha| {
        alpha.as_str().parse::<f64>().unwrap_or(1.0).min(1.0)
    });
    Some(Color { r, g, b, a })
}

fn flatten(color: Color, backdrop: Color) -> Color {
    if color.a >= 1.0 {
        return color;
    }
    let mix = |top: f64, bottom: f64| (top * color.a + bottom * (1.0 - color.a)).round();
    Color {
        r: mix(color.r, backdrop.r),
        g: mix(color.g, backdrop.g),
        b: mix(color.b, backdrop.b),
        a: 1.0,
    }
}

fn to_hex(color: Color) -> String {
    format!(
        "#{:02x}{:02x}{:02x}",
        color.r as u8, color.g as u8, color.b as u8
    )
}

/// Normalize a full palette, in any notation a scheme file or a built-in theme uses, to opaque
/// `#rrggbb`. A translucent color is flattened over the background — the built-in selections are
/// translucent, and that is the color the terminal paints. The cursor and the selection are
/// optional in most formats and derive from the text color when a scheme leaves them out.
fn opaque_scheme_colors(
    source: impl Fn(&str) -> Option<Value>,
    scheme_name: &str,
) -> Result<CustomSchemeColors, SchemeError> {
    let required = |key: &str| -> Result<Color, SchemeError> {
        parse_color(source(key).as_ref()).ok_or_else(|| {
            SchemeError(format!(
                "{scheme_name}: the color \"{}\" is missing or is not a color such as \"#1e1e2e\".",
                scheme_color_label(key)
            ))
        })
    };
    let background = Color {
        a: 1.0,
        ..required("background")?
    };
    let foreground = flatten(required("foreground")?, background);
    let cursor = flatten(
        parse_color(source("cursor").as_ref()).unwrap_or(foreground),
        background,
    );
    let selection = flatten(
        parse_color(source("selectionBackground").as_ref()).unwrap_or(Color {
            a: 0.3,
            ..foreground
        }),
        background,
    );
    let mut ansi = std::collections::HashMap::new();
    for key in SCHEME_ANSI_COLOR_KEYS
        .iter()
        .chain(SCHEME_BRIGHT_ANSI_COLOR_KEYS.iter())
    {
        ansi.insert(*key, to_hex(flatten(required(key)?, background)));
    }
    Ok(CustomSchemeColors::from_fn(|key| match key {
        "background" => to_hex(background),
        "foreground" => to_hex(foreground),
        "cursor" => to_hex(cursor),
        "selectionBackground" => to_hex(selection),
        other => ansi.remove(other).unwrap_or_default(),
    }))
}

/// Whether black text reads better than white on this `#rrggbb` background.
fn is_light_color(color: &str) -> bool {
    let channel = |offset: usize| {
        let value = f64::from(
            u8::from_str_radix(color.get(offset..offset + 2).unwrap_or("00"), 16).unwrap_or(0),
        ) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    };
    // The WCAG relative luminance at which both contrast ratios are equal.
    0.2126 * channel(1) + 0.7152 * channel(3) + 0.0722 * channel(5) > 0.179
}

fn unused_scheme_name(name: &str, existing: &[CustomTerminalScheme]) -> String {
    let taken: std::collections::HashSet<String> = terminal_schemes()
        .iter()
        .map(|scheme| scheme.name.to_lowercase())
        .chain(existing.iter().map(|scheme| scheme.name.to_lowercase()))
        .collect();
    if !taken.contains(&name.to_lowercase()) {
        return name.to_string();
    }
    let mut suffix = 2;
    while taken.contains(&format!("{name} {suffix}").to_lowercase()) {
        suffix += 1;
    }
    format!("{name} {suffix}")
}

fn derived_scheme_name(value: Option<&Value>) -> Option<String> {
    let text = value?.as_str()?;
    let shortened: String = text.trim().chars().take(MAX_DERIVED_NAME_LENGTH).collect();
    let trimmed = shortened.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn scheme_name_from_filename(filename: &str) -> String {
    let stem = match filename.rfind('.') {
        Some(dot) => &filename[..dot],
        None => filename,
    };
    derived_scheme_name(Some(&Value::from(stem))).unwrap_or_else(|| "Imported scheme".into())
}

/// Strip unknown JSON fields instead of carrying them into preferences and later exports.
fn copy_scheme(scheme: &CustomTerminalScheme) -> CustomTerminalScheme {
    CustomTerminalScheme {
        id: scheme.id.clone(),
        name: scheme.name.clone(),
        colors: CustomSchemeColors::from_fn(|key| {
            scheme.colors.get(key).unwrap_or_default().to_lowercase()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::palette::{terminal_scheme, terminal_scheme_id_for_host};
    use serde_json::json;

    fn frappe_colors() -> Value {
        json!({
            "background": "#303446", "foreground": "#c6d0f5", "cursor": "#f2d5cf",
            "selectionBackground": "#626880", "black": "#51576d", "red": "#e78284",
            "green": "#a6d189", "yellow": "#e5c890", "blue": "#8caaee", "magenta": "#f4b8e4",
            "cyan": "#81c8be", "white": "#b5bfe2", "brightBlack": "#626880", "brightRed": "#e78284",
            "brightGreen": "#a6d189", "brightYellow": "#e5c890", "brightBlue": "#8caaee",
            "brightMagenta": "#f4b8e4", "brightCyan": "#81c8be", "brightWhite": "#a5adce",
        })
    }

    fn frappe_json() -> Value {
        json!({ "id": "scheme-frappe", "name": "Catppuccin Frappé", "colors": frappe_colors() })
    }

    fn frappe() -> CustomTerminalScheme {
        serde_json::from_value(frappe_json()).unwrap()
    }

    /// Catppuccin Frappé as its Windows Terminal port ships it.
    fn windows_terminal_frappe() -> Value {
        json!({
            "name": "Catppuccin Frappe", "cursorColor": "#F2D5CF", "selectionBackground": "#626880",
            "background": "#303446", "foreground": "#C6D0F5", "black": "#51576D", "red": "#E78284",
            "green": "#A6D189", "yellow": "#E5C890", "blue": "#8CAAEE", "purple": "#F4B8E4",
            "cyan": "#81C8BE", "white": "#B5BFE2", "brightBlack": "#626880", "brightRed": "#E78284",
            "brightGreen": "#A6D189", "brightYellow": "#E5C890", "brightBlue": "#8CAAEE",
            "brightPurple": "#F4B8E4", "brightCyan": "#81C8BE", "brightWhite": "#A5ADCE",
        })
    }

    fn with(mut value: Value, key: &str, replacement: Value) -> Value {
        if replacement.is_null() {
            value.as_object_mut().unwrap().remove(key);
        } else {
            value[key] = replacement;
        }
        value
    }

    const ITERM_HEX: [&str; 16] = [
        "#51576d", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#b5bfe2",
        "#626880", "#e78284", "#a6d189", "#e5c890", "#8caaee", "#f4b8e4", "#81c8be", "#a5adce",
    ];

    fn iterm_names() -> Vec<(String, &'static str)> {
        let mut names = vec![
            ("Background Color".to_string(), "#303446"),
            ("Foreground Color".to_string(), "#c6d0f5"),
            ("Cursor Color".to_string(), "#f2d5cf"),
            ("Selection Color".to_string(), "#626880"),
        ];
        names.extend(
            ITERM_HEX
                .iter()
                .enumerate()
                .map(|(i, c)| (format!("Ansi {i} Color"), *c)),
        );
        names
    }

    /// JavaScript's shortest round-trip rendering of a component, as the TS test writes it.
    fn js_float(value: f64) -> String {
        if value == value.trunc() {
            format!("{value}")
        } else {
            format!("{value:?}")
        }
    }

    fn iterm_color(name: &str, hex: &str, alpha: f64) -> String {
        let component = |offset: usize| {
            f64::from(u8::from_str_radix(&hex[offset..offset + 2], 16).unwrap()) / 255.0
        };
        format!(
            "\t<key>{name}</key>\n\t<dict>\n\t\t<key>Alpha Component</key>\n\t\t<real>{}</real>\n\t\t<key>Blue Component</key>\n\t\t<real>{}</real>\n\t\t<key>Color Space</key>\n\t\t<string>sRGB</string>\n\t\t<key>Green Component</key>\n\t\t<real>{}</real>\n\t\t<key>Red Component</key>\n\t\t<real>{}</real>\n\t</dict>",
            js_float(alpha),
            js_float(component(5)),
            js_float(component(3)),
            js_float(component(1)),
        )
    }

    fn iterm_preset(colors: &[String]) -> String {
        format!(
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n{}\n</dict>\n</plist>\n",
            colors.join("\n")
        )
    }

    fn message<T: fmt::Debug>(result: Result<T, SchemeError>) -> String {
        result.unwrap_err().0
    }

    #[test]
    fn accepts_a_complete_scheme_and_rejects_malformed_ones() {
        assert!(is_custom_terminal_scheme_array(&json!([])));
        assert!(is_custom_terminal_scheme_array(&json!([frappe_json()])));

        assert!(!is_custom_terminal_scheme_array(&json!({})));
        assert!(!is_custom_terminal_scheme_array(&json!([
            frappe_json(),
            frappe_json()
        ])));
        assert!(!is_custom_terminal_scheme_array(&json!([with(
            frappe_json(),
            "name",
            json!("  ")
        )])));
        assert!(!is_custom_terminal_scheme_array(&json!([with(
            frappe_json(),
            "id",
            json!("x".repeat(65))
        )])));
        let bad_red = with(frappe_colors(), "red", json!("red"));
        assert!(!is_custom_terminal_scheme_array(&json!([with(
            frappe_json(),
            "colors",
            bad_red
        )])));
        let without_cursor = with(frappe_colors(), "cursor", Value::Null);
        assert!(!is_custom_terminal_scheme_array(&json!([with(
            frappe_json(),
            "colors",
            without_cursor
        )])));
        let extra = with(frappe_colors(), "extra", json!("#000000"));
        assert!(!is_custom_terminal_scheme_array(&json!([with(
            frappe_json(),
            "colors",
            extra
        )])));
        let too_many: Vec<Value> = (0..=MAX_CUSTOM_TERMINAL_SCHEMES)
            .map(|index| with(frappe_json(), "id", json!(format!("scheme-{index}"))))
            .collect();
        assert!(!is_custom_terminal_scheme_array(&Value::Array(too_many)));
    }

    #[test]
    fn never_lets_a_custom_scheme_take_a_built_in_id() {
        assert!(!is_custom_terminal_scheme_array(&json!([with(
            frappe_json(),
            "id",
            json!("dracula")
        )])));
    }

    #[test]
    fn turns_a_custom_scheme_into_a_terminal_theme() {
        let scheme = terminal_scheme_from_custom(&frappe());
        assert_eq!(scheme.id, "scheme-frappe");
        assert_eq!(scheme.name, "Catppuccin Frappé");
        assert!(!scheme.light);
        let colors = frappe_colors();
        for key in SCHEME_COLOR_KEYS {
            assert_eq!(scheme.theme.color(key), colors[key].as_str(), "{key}");
        }
        assert_eq!(scheme.theme.cursor_accent, "#303446");
        assert_eq!(
            scheme.theme.overview_ruler_border.as_deref(),
            Some("#00000000")
        );
    }

    #[test]
    fn classifies_a_scheme_by_its_background_as_the_built_in_schemes_are() {
        for builtin in terminal_schemes() {
            let copy = custom_scheme_copy_of(builtin, &[]).unwrap();
            assert_eq!(
                terminal_scheme_from_custom(&copy).light,
                builtin.light,
                "{}",
                builtin.name
            );
        }
    }

    #[test]
    fn keeps_the_converted_scheme_stable_until_its_stored_scheme_changes() {
        let other = CustomTerminalScheme {
            id: "scheme-other".into(),
            name: "Other".into(),
            ..frappe()
        };
        let converted = terminal_schemes_from_custom(&[frappe(), other.clone()]);
        assert_eq!(
            terminal_schemes_from_custom(&[frappe(), other.clone()]),
            converted
        );
        let edited = [
            frappe(),
            CustomTerminalScheme {
                name: "Renamed".into(),
                ..other
            },
        ];
        let reconverted = terminal_schemes_from_custom(&edited);
        assert_eq!(reconverted[0], converted[0]);
        assert_ne!(reconverted[1], converted[1]);
        assert_eq!(reconverted[1].name, "Renamed");
    }

    #[test]
    fn resolves_custom_scheme_ids_next_to_the_built_in_ones() {
        let custom = terminal_schemes_from_custom(&[frappe()]);
        assert_eq!(
            terminal_scheme(Some("scheme-frappe"), &custom).name,
            "Catppuccin Frappé"
        );
        assert_eq!(terminal_scheme(Some("dracula"), &custom).id, "dracula");
        assert_eq!(
            terminal_scheme(Some("scheme-deleted"), &custom).id,
            "vscode-dark"
        );
        assert_eq!(
            terminal_scheme(Some("scheme-frappe"), &[]).id,
            "vscode-dark"
        );
        assert_eq!(
            terminal_scheme_id_for_host("paper", Some("scheme-frappe"), &custom),
            "scheme-frappe"
        );
        assert_eq!(
            terminal_scheme_id_for_host("paper", Some("scheme-deleted"), &custom),
            "paper"
        );
    }

    #[test]
    fn starts_a_custom_scheme_from_a_built_in_one_under_a_free_name() {
        let mocha = terminal_scheme(Some("catppuccin-mocha"), &[]);
        let copy = custom_scheme_copy_of(mocha, &[]).unwrap();
        assert_eq!(copy.name, "Catppuccin Mocha copy");
        assert_ne!(copy.id, mocha.id);
        assert_eq!(copy.colors.background, "#1e1e2e");
        assert_eq!(copy.colors.magenta, "#f5c2e7");
        assert!(custom_terminal_schemes_valid(std::slice::from_ref(&copy)));

        let second = custom_scheme_copy_of(mocha, std::slice::from_ref(&copy)).unwrap();
        assert_eq!(second.name, "Catppuccin Mocha copy 2");
        assert_ne!(second.id, copy.id);
    }

    #[test]
    fn flattens_a_translucent_selection_over_the_background() {
        // Mocha selects with rgba(88, 91, 112, 0.6) on #1e1e2e.
        let copy =
            custom_scheme_copy_of(terminal_scheme(Some("catppuccin-mocha"), &[]), &[]).unwrap();
        assert_eq!(copy.colors.selection_background, "#414356");
    }

    #[test]
    fn round_trips_an_export() {
        let document = create_terminal_scheme_document(&[frappe()]).unwrap();
        assert_eq!(document.format, TERMINAL_SCHEME_FILE_FORMAT);
        let text = serde_json::to_string(&document).unwrap();
        assert_eq!(
            parse_terminal_scheme_file(&text, "export.json").unwrap(),
            vec![frappe()]
        );
    }

    #[test]
    fn refuses_to_export_nothing() {
        assert!(message(create_terminal_scheme_document(&[])).contains("at least one"));
    }

    #[test]
    fn rejects_an_export_from_a_newer_version_or_with_invalid_schemes() {
        let document =
            serde_json::to_value(create_terminal_scheme_document(&[frappe()]).unwrap()).unwrap();
        let newer = with(document.clone(), "version", json!(2));
        assert!(
            message(parse_terminal_scheme_file(
                &newer.to_string(),
                "export.json"
            ))
            .contains("version 2 is not supported")
        );
        let invalid = with(
            document,
            "schemes",
            json!([with(frappe_json(), "id", json!("nord"))]),
        );
        assert!(
            message(parse_terminal_scheme_file(
                &invalid.to_string(),
                "export.json"
            ))
            .contains("incomplete or invalid")
        );
    }

    #[test]
    fn imports_a_windows_terminal_color_scheme() {
        let schemes =
            parse_terminal_scheme_file(&windows_terminal_frappe().to_string(), "frappe.json")
                .unwrap();
        assert_eq!(schemes[0].name, "Catppuccin Frappe");
        assert_eq!(schemes[0].colors, frappe().colors);
        assert!(custom_terminal_schemes_valid(&schemes));
    }

    #[test]
    fn imports_every_scheme_of_a_windows_terminal_settings_file() {
        let unnamed = with(windows_terminal_frappe(), "name", Value::Null);
        let file = json!({ "schemes": [windows_terminal_frappe(), unnamed] });
        let schemes = parse_terminal_scheme_file(&file.to_string(), "settings.json").unwrap();
        let names: Vec<&str> = schemes.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["Catppuccin Frappe", "settings 2"]);
        assert_ne!(schemes[0].id, schemes[1].id);
    }

    #[test]
    fn derives_the_cursor_and_the_selection_when_a_scheme_leaves_them_out() {
        let bare = with(
            with(windows_terminal_frappe(), "cursorColor", Value::Null),
            "selectionBackground",
            Value::Null,
        );
        let schemes = parse_terminal_scheme_file(&bare.to_string(), "frappe.json").unwrap();
        assert_eq!(schemes[0].colors.cursor, "#c6d0f5");
        // The text color at 30% over the background.
        assert_eq!(schemes[0].colors.selection_background, "#5d637b");
    }

    #[test]
    fn reads_short_hex_and_rgb_notations() {
        let mut file = windows_terminal_frappe();
        file["black"] = json!("#000");
        file["white"] = json!("rgb(255, 255, 255)");
        file["selectionBackground"] = json!("rgba(255, 255, 255, 0.5)");
        let schemes = parse_terminal_scheme_file(&file.to_string(), "scheme.json").unwrap();
        assert_eq!(schemes[0].colors.black, "#000000");
        assert_eq!(schemes[0].colors.white, "#ffffff");
        assert_eq!(schemes[0].colors.selection_background, "#989aa3");
    }

    #[test]
    fn names_the_color_a_json_scheme_is_missing() {
        let incomplete = with(windows_terminal_frappe(), "purple", Value::Null);
        assert!(
            message(parse_terminal_scheme_file(
                &incomplete.to_string(),
                "frappe.json"
            ))
            .contains("Catppuccin Frappe: the color \"magenta (or purple)\" is missing")
        );
        assert!(
            message(parse_terminal_scheme_file(
                "{\"format\":\"muxus-backup\"}",
                "backup.json"
            ))
            .contains("backup: the color \"background\" is missing")
        );
    }

    #[test]
    fn imports_an_iterm2_preset_and_names_it_after_the_file() {
        let colors: Vec<String> = iterm_names()
            .iter()
            .map(|(name, hex)| iterm_color(name, hex, 1.0))
            .collect();
        let schemes =
            parse_terminal_scheme_file(&iterm_preset(&colors), "Catppuccin Frappe.itermcolors")
                .unwrap();
        assert_eq!(schemes[0].name, "Catppuccin Frappe");
        assert_eq!(schemes[0].colors, frappe().colors);
    }

    #[test]
    fn flattens_a_translucent_iterm2_selection_and_reports_a_missing_color() {
        let colors: Vec<String> = iterm_names()
            .iter()
            .map(|(name, hex)| {
                iterm_color(name, hex, if name == "Selection Color" { 0.5 } else { 1.0 })
            })
            .collect();
        let schemes =
            parse_terminal_scheme_file(&iterm_preset(&colors), "half.itermcolors").unwrap();
        assert_eq!(schemes[0].colors.selection_background, "#494e63");

        let without_red: Vec<String> = colors
            .into_iter()
            .filter(|color| !color.contains("<key>Ansi 1 Color</key>"))
            .collect();
        assert!(
            message(parse_terminal_scheme_file(
                &iterm_preset(&without_red),
                "half.itermcolors"
            ))
            .contains("half: the color \"red\" is missing")
        );
    }

    #[test]
    fn rejects_files_that_are_not_a_color_scheme() {
        assert!(message(parse_terminal_scheme_file("not json", "notes.txt")).contains("neither"));
        assert!(
            message(parse_terminal_scheme_file("<html></html>", "page.html")).contains("neither")
        );
        assert!(
            message(parse_terminal_scheme_file("[]", "empty.json")).contains("no color schemes")
        );
        assert!(
            message(parse_terminal_scheme_file("[42]", "numbers.json"))
                .contains("does not describe")
        );
    }

    #[test]
    fn replaces_a_scheme_with_a_known_id_in_place_and_keeps_the_others() {
        let other = CustomTerminalScheme {
            id: "scheme-other".into(),
            name: "Other".into(),
            ..frappe()
        };
        let updated = CustomTerminalScheme {
            name: "Frappé, retuned".into(),
            ..frappe()
        };
        assert_eq!(
            merge_custom_terminal_schemes(
                &[frappe(), other.clone()],
                std::slice::from_ref(&updated)
            )
            .unwrap(),
            vec![updated, other]
        );
    }

    #[test]
    fn adds_an_unknown_scheme_under_a_name_that_is_still_free() {
        let again = CustomTerminalScheme {
            id: "scheme-again".into(),
            ..frappe()
        };
        let dracula = CustomTerminalScheme {
            id: "scheme-dracula".into(),
            name: "Dracula".into(),
            ..frappe()
        };
        let merged = merge_custom_terminal_schemes(&[frappe()], &[again, dracula]).unwrap();
        let names: Vec<&str> = merged.iter().map(|s| s.name.as_str()).collect();
        // The built-in scheme already uses the plain name.
        assert_eq!(
            names,
            ["Catppuccin Frappé", "Catppuccin Frappé 2", "Dracula 2"]
        );
        assert!(custom_terminal_schemes_valid(&merged));
    }

    #[test]
    fn refuses_an_import_beyond_the_scheme_limit() {
        let full: Vec<CustomTerminalScheme> = (0..MAX_CUSTOM_TERMINAL_SCHEMES)
            .map(|index| CustomTerminalScheme {
                id: format!("scheme-{index}"),
                name: format!("Scheme {index}"),
                ..frappe()
            })
            .collect();
        assert!(
            message(merge_custom_terminal_schemes(&full, &[frappe()])).contains("exceed the limit")
        );
    }
}
