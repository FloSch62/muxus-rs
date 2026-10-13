//! Clickable links in terminal output: web URLs (xterm's `WebLinksAddon`), file paths
//! (`client/src/terminal/file-links.ts`) and the gesture that opens them
//! (`file-link-activation.ts`, `web-links.ts`).
//!
//! The URL pattern and the string-to-cell mapping are adapted from xterm.js's `WebLinksAddon`
//! (MIT).

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::buffer::{TextBuffer, translate_to_string};
use crate::selection::BufferPoint;

// ---------------------------------------------------------------------------------------------
// Activation gestures
// ---------------------------------------------------------------------------------------------

/// The mouse gesture that opens a detected file path or web URL.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LinkActivation {
    Direct,
    #[default]
    Alt,
    Ctrl,
    Meta,
}

/// The pointer state a link click is judged by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LinkClick {
    /// 0 = primary, 1 = middle, 2 = secondary.
    pub button: u8,
    pub alt: bool,
    pub ctrl: bool,
    pub meta: bool,
    pub shift: bool,
}

/// Mouse gestures that can actually be produced on the current platform, with their labels.
pub fn link_activation_options(is_mac: bool) -> [(LinkActivation, &'static str); 3] {
    if is_mac {
        [
            (LinkActivation::Alt, "Option + left click"),
            (LinkActivation::Meta, "Cmd + left click"),
            (LinkActivation::Direct, "Direct left click"),
        ]
    } else {
        [
            (LinkActivation::Alt, "Alt + left click"),
            (LinkActivation::Ctrl, "Ctrl + left click"),
            (LinkActivation::Direct, "Direct left click"),
        ]
    }
}

/// Map a cross-platform backup to a gesture that is safe on this platform.
pub fn link_activation_for_platform(activation: LinkActivation, is_mac: bool) -> LinkActivation {
    match activation {
        LinkActivation::Meta if !is_mac => LinkActivation::Alt,
        LinkActivation::Ctrl if is_mac => LinkActivation::Meta,
        other => other,
    }
}

/// Match only the configured left-click gesture, leaving every other click to the terminal.
pub fn link_activation_matches(click: LinkClick, activation: LinkActivation) -> bool {
    if click.button != 0 || click.shift {
        return false;
    }
    match activation {
        LinkActivation::Direct => !click.alt && !click.ctrl && !click.meta,
        LinkActivation::Alt => click.alt && !click.ctrl && !click.meta,
        LinkActivation::Ctrl => click.ctrl && !click.alt && !click.meta,
        LinkActivation::Meta => click.meta && !click.alt && !click.ctrl,
    }
}

/// Keep Alt+click cursor movement from competing with an Alt file-open gesture.
pub fn alt_click_moves_cursor(activation: LinkActivation, is_mac: bool) -> bool {
    link_activation_for_platform(activation, is_mac) != LinkActivation::Alt
}

/// The URL a web link opens, normalized; `None` for anything but http and https (a terminal must
/// never open `file:` or custom schemes on a click).
pub fn web_link_target(uri: &str) -> Option<String> {
    let parsed = url::Url::parse(uri).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

// ---------------------------------------------------------------------------------------------
// Web links
// ---------------------------------------------------------------------------------------------

// Everything starting with http:// or https:// up to the first whitespace, `"` or `'`, excluding
// unsafe characters and, as final characters, interpunction and brackets.
static STRICT_URL_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(https?|HTTPS?):[/]{2}[^\s"'!*(){}|\\\^<>`]*[^\s"':,.!?{}|\\\^~\[\]`()<>]"#)
        .unwrap()
});

/// Whether the matched text forms a proper URL whose authority is spelled as written.
fn is_url(text: &str) -> bool {
    let Ok(url) = url::Url::parse(text) else {
        return false;
    };
    let host = match (url.host_str(), url.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        (None, _) => String::new(),
    };
    let base = match (url.username(), url.password()) {
        (user, Some(password)) if !user.is_empty() => {
            format!("{}://{user}:{password}@{host}", url.scheme())
        }
        (user, _) if !user.is_empty() => format!("{}://{user}@{host}", url.scheme()),
        _ => format!("{}://{host}", url.scheme()),
    };
    text.to_lowercase().starts_with(&base.to_lowercase())
}

/// URLs in one line of text, as byte ranges.
pub fn web_link_ranges(line: &str) -> Vec<(usize, usize)> {
    STRICT_URL_RE
        .find_iter(line)
        .filter(|found| is_url(found.as_str()))
        .map(|found| (found.start(), found.end()))
        .collect()
}

// ---------------------------------------------------------------------------------------------
// File links
// ---------------------------------------------------------------------------------------------

const MAX_LOGICAL_LINE_LENGTH: usize = 4_096;
const TRAILING_PUNCTUATION: [char; 4] = [',', ';', '!', '?'];
const UNSAFE_PATH_CHARACTERS: [char; 9] = ['*', '?', '[', ']', '{', '}', '<', '>', '|'];
const CONVENTIONAL_FILENAMES: [&str; 14] = [
    "brewfile",
    "cmakelists.txt",
    "compose.yaml",
    "compose.yml",
    "containerfile",
    "dockerfile",
    "gemfile",
    "justfile",
    "license",
    "makefile",
    "procfile",
    "rakefile",
    "readme",
    "vagrantfile",
];

// OCI image names overlap heavily with relative paths. Requiring a tag or digest keeps ordinary
// paths clickable while excluding the form users most commonly see in containerlab and Compose
// files.
static CONTAINER_IMAGE_REFERENCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(?:localhost|[a-z\d](?:[a-z\d.-]*[a-z\d])?)(?::\d+)?/)?(?:[a-z\d._-]+/)+[a-z\d._-]+(?::[a-z\d_][a-z\d._-]{0,127}|@sha256:[a-f\d]{32,})$").unwrap()
});
static REGISTRY_PARTS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([a-z\d](?:[a-z\d.-]*[a-z\d])?)(?::\d+)?$").unwrap());
static REGISTRY_PORT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r":\d+$").unwrap());
static IMAGE_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:[a-z\d._-]+/)*[a-z\d._-]+$").unwrap());
static URL_SCHEME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^[a-z][a-z\d+.-]*://").unwrap());
static VERSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^v?\d+(?:\.\d+){1,3}(?:[-+][\w.-]+)?$").unwrap());
static PATH_PREFIX: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:/|\.\.?/|~/)").unwrap());
static EXTENSION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\.[a-z\d][a-z\d+_-]{0,15}$").unwrap());
static COMPILER_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?):\d+(?::\d+)?(?::.*)?$").unwrap());
static PARENTHESIZED_LOCATION: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(.+?)\(\d+(?:,\d+)?\)$").unwrap());
static WINDOWS_DRIVE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([a-z]:)\\(.*)$").unwrap());
static WINDOWS_UNC: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\\\\([^\\]+)\\([^\\]+)(?:\\(.*))?$").unwrap());
static LS_MODE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[bcdlps-][rwxStTs-]{9}[.@+]?$").unwrap());
static SMALL_NUMBER: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{1,2}$").unwrap());
static TIME_OR_YEAR: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?:\d{1,2}:\d{2}(?::\d{2})?|\d{4})$").unwrap());
static ISO_DATE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d{4}-\d{2}-\d{2}$").unwrap());
static ISO_TIME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{2}:\d{2}(?::\d{2}(?:\.\d+)?)?$").unwrap());
static TIMEZONE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[+-]\d{4}$").unwrap());

/// A path-shaped token in a logical terminal line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileLinkCandidate {
    /// Decoded path passed to the path resolver.
    pub path: String,
    /// Byte offsets into the logical terminal line.
    pub start: usize,
    pub end: usize,
}

fn escaped_at(chars: &[char], index: usize) -> bool {
    let mut slashes = 0;
    let mut cursor = index;
    while cursor > 0 && chars[cursor - 1] == '\\' {
        slashes += 1;
        cursor -= 1;
    }
    slashes % 2 == 1
}

fn windows_absolute_path_root(value: &str) -> Option<(String, String)> {
    let normalized = value.replace('/', "\\");
    if let Some(drive) = WINDOWS_DRIVE.captures(&normalized) {
        return Some((format!("{}\\", &drive[1]), drive[2].to_string()));
    }
    let unc = WINDOWS_UNC.captures(&normalized)?;
    Some((
        format!("\\\\{}\\{}", &unc[1], &unc[2]),
        unc.get(3).map_or(String::new(), |m| m.as_str().into()),
    ))
}

fn decode_shell_escapes(value: &str) -> String {
    // Backslashes are separators rather than shell escapes in native Windows absolute paths.
    // Preserve them so the local-file resolver receives the path that was displayed.
    if windows_absolute_path_root(value).is_some() {
        return value.into();
    }
    let mut out = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some(next) => out.push(next),
                None => out.push(c),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn safe_path_value(value: &str) -> bool {
    !(value.is_empty()
        || value.encode_utf16().count() > 4_096
        || value == "."
        || value == ".."
        || value == "/"
        || value.contains(['\0', '\r', '\n'])
        || value.chars().any(|c| UNSAFE_PATH_CHARACTERS.contains(&c))
        || URL_SCHEME.is_match(value))
}

fn looks_like_container_image_reference(value: &str) -> bool {
    if CONTAINER_IMAGE_REFERENCE.is_match(value) {
        return true;
    }
    // A registry-qualified image is still an image when the implicit `latest` tag is omitted.
    // Restrict this form to an unmistakable registry host so a normal `directory/file` path
    // remains clickable.
    let Some(slash) = value.find('/').filter(|slash| *slash > 0) else {
        return false;
    };
    let registry = value[..slash].to_lowercase();
    let Some(parts) = REGISTRY_PARTS.captures(&registry) else {
        return false;
    };
    let host = &parts[1];
    if host != "localhost" && !host.contains('.') && !REGISTRY_PORT.is_match(&registry) {
        return false;
    }
    IMAGE_PATH.is_match(&value[slash + 1..])
}

fn looks_like_path(value: &str) -> bool {
    if !safe_path_value(value)
        || looks_like_container_image_reference(value)
        || VERSION.is_match(value)
    {
        return false;
    }
    if windows_absolute_path_root(value).is_some()
        || PATH_PREFIX.is_match(value)
        || value.contains('/')
    {
        return true;
    }
    if CONVENTIONAL_FILENAMES.contains(&value.to_lowercase().as_str()) {
        return true;
    }
    if value.starts_with('.') && value.chars().count() > 1 {
        return true;
    }
    EXTENSION.is_match(value)
}

fn wrapper_close(c: char) -> Option<char> {
    match c {
        '(' => Some(')'),
        '[' => Some(']'),
        '{' => Some('}'),
        '<' => Some('>'),
        _ => None,
    }
}

/// A candidate from `chars[start..end]` (char indices), mapped back to byte offsets in `line`.
fn candidate_from_range(
    line: &str,
    chars: &[char],
    offsets: &[usize],
    initial_start: usize,
    initial_end: usize,
) -> Option<FileLinkCandidate> {
    let mut start = initial_start;
    let mut end = initial_end;
    let mut closing: Vec<char> = Vec::new();
    while start < end {
        let Some(close) = wrapper_close(chars[start]) else {
            break;
        };
        closing.push(close);
        start += 1;
    }
    while end > start && TRAILING_PUNCTUATION.contains(&chars[end - 1]) {
        end -= 1;
    }
    while let Some(close) = closing.pop() {
        if end > start && chars[end - 1] == close {
            end -= 1;
        } else {
            break;
        }
    }
    if start >= end {
        return None;
    }
    let byte_start = offsets[start];
    let mut byte_end = offsets[end];
    let mut displayed = &line[byte_start..byte_end];
    if looks_like_container_image_reference(&decode_shell_escapes(displayed)) {
        return None;
    }
    if let Some(location) = COMPILER_LOCATION.captures(displayed) {
        let prefix = location.get(1).expect("group 1").as_str();
        byte_end = byte_start + prefix.len();
        displayed = &line[byte_start..byte_end];
    } else if let Some(location) = PARENTHESIZED_LOCATION.captures(displayed) {
        let prefix = location.get(1).expect("group 1").as_str();
        byte_end = byte_start + prefix.len();
        displayed = &line[byte_start..byte_end];
    } else if displayed.ends_with(':') {
        byte_end -= 1;
        displayed = &line[byte_start..byte_end];
    }
    let path = decode_shell_escapes(displayed);
    looks_like_path(&path).then_some(FileLinkCandidate {
        path,
        start: byte_start,
        end: byte_end,
    })
}

/// Recognize the date/time fields in GNU/BSD `ls -l` output and treat what follows as
/// authoritative. This keeps extensionless files clickable without mistaking values such as the
/// `3.8K` size column for filenames; finding the date also tolerates SELinux contexts and device
/// major/minor columns.
fn long_listing_file_candidate(line: &str) -> Option<Vec<FileLinkCandidate>> {
    let fields: Vec<(usize, &str)> = line
        .split_whitespace()
        .map(|field| (field.as_ptr() as usize - line.as_ptr() as usize, field))
        .collect();
    let mode = fields.first()?.1;
    if !LS_MODE.is_match(mode) {
        return None;
    }
    if !mode.starts_with('-') {
        return Some(Vec::new());
    }
    let mut filename_field = None;
    for index in 2..fields.len().saturating_sub(1) {
        let value = fields[index].1;
        let previous = fields[index - 1].1;
        let month_day_time = SMALL_NUMBER.is_match(previous) && TIME_OR_YEAR.is_match(value);
        let iso_time = ISO_DATE.is_match(previous) && ISO_TIME.is_match(value);
        if !month_day_time && !iso_time {
            continue;
        }
        let mut field = index + 1;
        if fields
            .get(field)
            .is_some_and(|(_, value)| TIMEZONE.is_match(value))
        {
            field += 1;
        }
        filename_field = Some(field);
        break;
    }
    let Some(field) = filename_field.filter(|field| *field < fields.len()) else {
        return Some(Vec::new());
    };
    let mut start = fields[field].0;
    let mut end = line.trim_end().len();
    let bytes = line.as_bytes();
    if (bytes[start] == b'\'' || bytes[start] == b'"') && bytes[end - 1] == bytes[start] {
        start += 1;
        end -= 1;
    }
    if start >= end {
        return Some(Vec::new());
    }
    let path = decode_shell_escapes(&line[start..end]);
    Some(if safe_path_value(&path) {
        vec![FileLinkCandidate { path, start, end }]
    } else {
        Vec::new()
    })
}

/// Find path-shaped tokens without turning ordinary terminal prose into links.
pub fn terminal_file_link_candidates(line: &str) -> Vec<FileLinkCandidate> {
    if let Some(long_listing) = long_listing_file_candidate(line) {
        return long_listing;
    }
    let chars: Vec<char> = line.chars().collect();
    let mut offsets: Vec<usize> = line.char_indices().map(|(offset, _)| offset).collect();
    offsets.push(line.len());

    let mut candidates = Vec::new();
    let mut cursor = 0;
    while cursor < chars.len() {
        while cursor < chars.len() && chars[cursor].is_whitespace() {
            cursor += 1;
        }
        if cursor >= chars.len() {
            break;
        }
        let token_start = cursor;
        let quote = (chars[cursor] == '\'' || chars[cursor] == '"').then_some(chars[cursor]);
        if let Some(quote) = quote {
            cursor += 1;
            let content_start = cursor;
            while cursor < chars.len() && (chars[cursor] != quote || escaped_at(&chars, cursor)) {
                cursor += 1;
            }
            if let Some(candidate) =
                candidate_from_range(line, &chars, &offsets, content_start, cursor)
            {
                candidates.push(candidate);
            }
            while cursor < chars.len() && !chars[cursor].is_whitespace() {
                cursor += 1;
            }
            continue;
        }
        while cursor < chars.len() {
            if chars[cursor].is_whitespace() && !escaped_at(&chars, cursor) {
                break;
            }
            cursor += 1;
        }
        if let Some(candidate) = candidate_from_range(line, &chars, &offsets, token_start, cursor) {
            candidates.push(candidate);
        }
    }
    candidates
}

fn normalize_absolute_path(value: &str) -> String {
    let mut segments: Vec<&str> = Vec::new();
    for segment in value.split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    format!("/{}", segments.join("/"))
}

fn normalize_windows_absolute_path(value: &str) -> Option<String> {
    let (root, remainder) = windows_absolute_path_root(value)?;
    let mut segments: Vec<&str> = Vec::new();
    for segment in remainder.split('\\') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop();
            }
            other => segments.push(other),
        }
    }
    if segments.is_empty() {
        return Some(root);
    }
    let separator = if root.ends_with('\\') { "" } else { "\\" };
    Some(format!("{root}{separator}{}", segments.join("\\")))
}

/// Which file system a terminal path belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PathSource {
    /// A remote POSIX host, reached over SFTP.
    #[default]
    Posix,
    /// The local machine, which may be Windows.
    Local,
}

/// Resolve a terminal path before handing it to the local editor or SFTP.
pub fn resolve_terminal_file_path(
    candidate: &str,
    cwd: Option<&str>,
    home: Option<&str>,
    source: PathSource,
) -> Option<String> {
    if candidate.is_empty() || candidate.contains(['\0', '\r', '\n']) {
        return None;
    }
    if candidate == "~" || candidate.starts_with("~/") {
        let rest = candidate.get(2..).unwrap_or("");
        if source == PathSource::Local
            && let Some(home) = home.filter(|home| windows_absolute_path_root(home).is_some())
        {
            return normalize_windows_absolute_path(&format!("{home}\\{rest}"));
        }
        let home = home.filter(|home| home.starts_with('/'))?;
        return Some(normalize_absolute_path(&format!("{home}/{rest}")));
    }
    if candidate.starts_with('~') {
        return None;
    }
    if windows_absolute_path_root(candidate).is_some() {
        return if source == PathSource::Local {
            normalize_windows_absolute_path(candidate)
        } else {
            None
        };
    }
    if candidate.starts_with('/') {
        return Some(normalize_absolute_path(candidate));
    }
    if source == PathSource::Local
        && let Some(cwd) = cwd.filter(|cwd| windows_absolute_path_root(cwd).is_some())
    {
        return normalize_windows_absolute_path(&format!("{cwd}\\{candidate}"));
    }
    let cwd = cwd.filter(|cwd| cwd.starts_with('/'))?;
    Some(normalize_absolute_path(&format!("{cwd}/{candidate}")))
}

// ---------------------------------------------------------------------------------------------
// Links in a buffer
// ---------------------------------------------------------------------------------------------

/// What a link opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LinkTarget {
    /// An http(s) URL, as matched.
    Web(String),
    /// A path as displayed (decoded); resolve it with [`resolve_terminal_file_path`].
    File(String),
}

/// A link found in the buffer, with the cells it covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BufferLink {
    pub target: LinkTarget,
    /// First cell of the link.
    pub start: BufferPoint,
    /// Last cell of the link (inclusive).
    pub end: BufferPoint,
    /// The text as displayed.
    pub text: String,
}

impl BufferLink {
    pub fn contains(&self, point: BufferPoint) -> bool {
        point >= self.start && point <= self.end
    }
}

fn line_string<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> Option<String> {
    buffer
        .line_cells(line)
        .map(|cells| translate_to_string(&cells, true, 0, None))
}

/// The logical line around `line`: wrapped rows above and below joined, as file links read it.
fn logical_line_at<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> Option<(String, usize)> {
    let mut start_line = line;
    line_string(buffer, start_line)?;
    let mut scanned = 0;
    while buffer.is_wrapped(start_line) && scanned < MAX_LOGICAL_LINE_LENGTH && start_line > 0 {
        let Some(previous) = line_string(buffer, start_line - 1) else {
            break;
        };
        scanned += previous.encode_utf16().count();
        start_line -= 1;
    }
    let mut parts = String::new();
    let mut index = start_line;
    while let Some(content) = line_string(buffer, index) {
        scanned += content.encode_utf16().count();
        parts.push_str(&content);
        index += 1;
        if index >= buffer.length()
            || !buffer.is_wrapped(index)
            || scanned >= MAX_LOGICAL_LINE_LENGTH
        {
            break;
        }
    }
    Some((parts, start_line))
}

/// The window of wrapped rows the web-link matcher reads around `line` (xterm's
/// `_getWindowedLineStrings`): it stops at rows with spaces or after 2048 characters.
fn windowed_line<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> Option<(String, usize)> {
    let current = line_string(buffer, line)?;
    let mut lines: Vec<String> = Vec::new();
    let mut top = line;
    if buffer.is_wrapped(line) && !current.starts_with(' ') {
        let mut length = 0;
        while top > 0 && length < 2048 {
            let Some(content) = line_string(buffer, top - 1) else {
                break;
            };
            top -= 1;
            length += content.encode_utf16().count();
            let stop = !buffer.is_wrapped(top) || content.contains(' ');
            lines.push(content);
            if stop {
                break;
            }
        }
        lines.reverse();
    }
    lines.push(current);
    let mut bottom = line + 1;
    let mut length = 0;
    while bottom < buffer.length() && buffer.is_wrapped(bottom) && length < 2048 {
        let Some(content) = line_string(buffer, bottom) else {
            break;
        };
        length += content.encode_utf16().count();
        let stop = content.contains(' ');
        lines.push(content);
        bottom += 1;
        if stop {
            break;
        }
    }
    Some((lines.concat(), top))
}

/// Map a byte offset in a joined, right-trimmed line string back to a cell, xterm's `_mapStrIdx`:
/// blank cells count one byte (the space they read as), and an empty last cell before a wrapped
/// wide character — trimmed from the string — is corrected for.
fn buffer_position<B: TextBuffer + ?Sized>(
    buffer: &B,
    initial_line: usize,
    offset: usize,
) -> Option<BufferPoint> {
    let mut remaining = offset as isize;
    let mut line = initial_line;
    let mut column = 0;
    while remaining > 0 {
        let cells = buffer.line_cells(line)?;
        for (index, cell) in cells.iter().enumerate().skip(column) {
            if cell.width > 0 {
                remaining -= cell.text.len().max(1) as isize;
                // A wide character split at the right edge leaves a blank cell followed by its
                // width-two cell on the wrapped line.
                if index == cells.len() - 1
                    && cell.text.is_empty()
                    && line + 1 < buffer.length()
                    && buffer.is_wrapped(line + 1)
                    && buffer
                        .line_cells(line + 1)
                        .and_then(|next| next.first().map(|first| first.width == 2))
                        == Some(true)
                {
                    remaining += 1;
                }
            }
            if remaining < 0 {
                return Some(BufferPoint::new(line, index));
            }
        }
        line += 1;
        column = 0;
    }
    Some(BufferPoint::new(line, column))
}

/// The web links on the logical line containing `line`.
pub fn web_links_at<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> Vec<BufferLink> {
    let Some((text, top)) = windowed_line(buffer, line) else {
        return Vec::new();
    };
    web_link_ranges(&text)
        .into_iter()
        .filter_map(|(start, end)| {
            let first = buffer_position(buffer, top, start)?;
            let last = buffer_position(buffer, top, end - 1)?;
            Some(BufferLink {
                target: LinkTarget::Web(text[start..end].to_string()),
                start: first,
                end: last,
                text: text[start..end].to_string(),
            })
        })
        .collect()
}

/// The file links on the logical line containing `line`.
pub fn file_links_at<B: TextBuffer + ?Sized>(buffer: &B, line: usize) -> Vec<BufferLink> {
    let Some((text, start_line)) = logical_line_at(buffer, line) else {
        return Vec::new();
    };
    terminal_file_link_candidates(&text)
        .into_iter()
        .filter_map(|candidate| {
            let first = buffer_position(buffer, start_line, candidate.start)?;
            // Map the final character rather than the exclusive end, which rolls over to the next
            // row when the path ends in the last cell.
            let last_char = text[..candidate.end].chars().next_back()?.len_utf8();
            let last = buffer_position(buffer, start_line, candidate.end - last_char)?;
            Some(BufferLink {
                text: text[candidate.start..candidate.end].to_string(),
                target: LinkTarget::File(candidate.path),
                start: first,
                end: last,
            })
        })
        .collect()
}

/// The link under a cell: web links first, then file links when `file_links` is enabled for the
/// session (local shells, and SSH sessions with SFTP).
pub fn link_at<B: TextBuffer + ?Sized>(
    buffer: &B,
    point: BufferPoint,
    file_links: bool,
) -> Option<BufferLink> {
    if let Some(link) = web_links_at(buffer, point.line)
        .into_iter()
        .find(|link| link.contains(point))
    {
        return Some(link);
    }
    if !file_links {
        return None;
    }
    file_links_at(buffer, point.line)
        .into_iter()
        .find(|link| link.contains(point))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::buffer::StringBuffer;

    fn paths(line: &str) -> Vec<String> {
        terminal_file_link_candidates(line)
            .into_iter()
            .map(|candidate| candidate.path)
            .collect()
    }

    #[test]
    fn only_offers_cmd_activation_on_macos() {
        assert_eq!(
            link_activation_options(false).map(|(value, _)| value),
            [
                LinkActivation::Alt,
                LinkActivation::Ctrl,
                LinkActivation::Direct
            ]
        );
        assert_eq!(
            link_activation_options(true).map(|(value, _)| value),
            [
                LinkActivation::Alt,
                LinkActivation::Meta,
                LinkActivation::Direct
            ]
        );
    }

    #[test]
    fn maps_cross_platform_modifier_preferences_to_safe_native_gestures() {
        assert_eq!(
            link_activation_for_platform(LinkActivation::Meta, false),
            LinkActivation::Alt
        );
        assert_eq!(
            link_activation_for_platform(LinkActivation::Meta, true),
            LinkActivation::Meta
        );
        assert_eq!(
            link_activation_for_platform(LinkActivation::Ctrl, true),
            LinkActivation::Meta
        );
        assert_eq!(
            link_activation_for_platform(LinkActivation::Ctrl, false),
            LinkActivation::Ctrl
        );
    }

    #[test]
    fn disables_cursor_movement_only_while_alt_opens_file_links() {
        assert!(!alt_click_moves_cursor(LinkActivation::Alt, false));
        assert!(alt_click_moves_cursor(LinkActivation::Ctrl, false));
        assert!(alt_click_moves_cursor(LinkActivation::Meta, true));
        assert!(alt_click_moves_cursor(LinkActivation::Direct, false));
    }

    #[test]
    fn finds_absolute_relative_dotfile_and_conventional_file_paths() {
        assert_eq!(
            paths("edit /etc/hosts ./src/App.tsx ../shared/api.ts .env Dockerfile README"),
            [
                "/etc/hosts",
                "./src/App.tsx",
                "../shared/api.ts",
                ".env",
                "Dockerfile",
                "README"
            ]
        );
    }

    #[test]
    fn supports_quoted_and_shell_escaped_paths_with_spaces() {
        assert_eq!(
            paths(r#""/srv/my app/main.ts" './docs/user guide.md' src/a\ file.ts"#),
            [
                "/srv/my app/main.ts",
                "./docs/user guide.md",
                "src/a file.ts"
            ]
        );
    }

    #[test]
    fn recognizes_native_windows_drive_letter_and_unc_paths_without_stripping_separators() {
        assert_eq!(
            paths(r"C:\Users\me\foo.txt C:/Users/me/bar.txt \\server\share\baz.txt"),
            [
                r"C:\Users\me\foo.txt",
                "C:/Users/me/bar.txt",
                r"\\server\share\baz.txt"
            ]
        );
    }

    #[test]
    fn strips_compiler_locations_and_surrounding_punctuation_from_the_linked_range() {
        let line = "error: (src/main.ts:42:7), config.yaml(8,2) README.md:";
        let candidates = terminal_file_link_candidates(line);
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.path.as_str())
                .collect::<Vec<_>>(),
            ["src/main.ts", "config.yaml", "README.md"]
        );
        assert_eq!(
            candidates
                .iter()
                .map(|c| &line[c.start..c.end])
                .collect::<Vec<_>>(),
            ["src/main.ts", "config.yaml", "README.md"]
        );
    }

    #[test]
    fn does_not_link_urls_flags_versions_or_ordinary_prose() {
        assert!(
            paths("open https://example.test/file.txt --config v1.2.3 ordinary words").is_empty()
        );
    }

    #[test]
    fn does_not_confuse_tagged_container_image_references_with_file_paths() {
        assert!(paths("image: ghcr.io/nokia/srlinux:latest ghcr.io/srl-labs/network-multitool:latest ghcr.io/nokia/srlinux").is_empty());
        assert!(paths("image: srl-labs/network-multitool:latest").is_empty());
        assert!(paths("image: srl-labs/network-multitool:42").is_empty());
    }

    #[test]
    fn recognizes_only_the_regular_filename_in_long_ls_output() {
        assert_eq!(
            paths("-rw-rw-rw-.  1 root root 3.8K Jul 23 16:33 containerlab.svg"),
            ["containerlab.svg"]
        );
        assert_eq!(
            paths("-rw-r--r--.  1 root root 406K Feb 27  2025 hist"),
            ["hist"]
        );
        assert!(paths("drwxr-xr-x.  5 root root  100 May  4  2024 demo").is_empty());
        assert_eq!(
            paths(
                "-rw-r--r--. 1 root root system_u:object_r:admin_home_t:s0 928 Dec 1 2025 lic.txt"
            ),
            ["lic.txt"]
        );
        assert_eq!(
            paths("-rw-r--r-- 1 root root 928 2025-12-01 09:42 +0100 lic.txt"),
            ["lic.txt"]
        );
    }

    #[test]
    fn maps_a_detected_path_to_its_cells() {
        let buffer = StringBuffer::new(20, &[("output: src/main.ts", false)]);
        let links = file_links_at(&buffer, 0);
        assert_eq!(links.len(), 1);
        // xterm reports 1-based x 9..19 for this range.
        assert_eq!(
            (links[0].start, links[0].end),
            (BufferPoint::new(0, 8), BufferPoint::new(0, 18))
        );
        assert_eq!(links[0].target, LinkTarget::File("src/main.ts".into()));
    }

    #[test]
    fn keeps_a_link_ending_in_the_last_column_on_the_current_row() {
        let text = "output: src/main.ts";
        let buffer = StringBuffer::new(text.len(), &[(text, false), ("next", false)]);
        let links = file_links_at(&buffer, 0);
        assert_eq!(links[0].end, BufferPoint::new(0, text.len() - 1));
    }

    #[test]
    fn follows_a_path_across_a_soft_wrap() {
        let buffer = StringBuffer::new(10, &[("cat /etc/h", false), ("osts", true)]);
        let links = file_links_at(&buffer, 1);
        assert_eq!(links[0].target, LinkTarget::File("/etc/hosts".into()));
        assert_eq!(
            (links[0].start, links[0].end),
            (BufferPoint::new(0, 4), BufferPoint::new(1, 3))
        );
    }

    #[test]
    fn opens_a_link_only_for_the_configured_gesture() {
        let cases = [
            (
                LinkActivation::Direct,
                LinkClick::default(),
                LinkClick {
                    alt: true,
                    ..Default::default()
                },
            ),
            (
                LinkActivation::Alt,
                LinkClick {
                    alt: true,
                    ..Default::default()
                },
                LinkClick::default(),
            ),
            (
                LinkActivation::Ctrl,
                LinkClick {
                    ctrl: true,
                    ..Default::default()
                },
                LinkClick::default(),
            ),
            (
                LinkActivation::Meta,
                LinkClick {
                    meta: true,
                    ..Default::default()
                },
                LinkClick::default(),
            ),
        ];
        for (activation, matching, other) in cases {
            assert!(!link_activation_matches(
                LinkClick {
                    button: 2,
                    ..matching
                },
                activation
            ));
            assert!(!link_activation_matches(other, activation));
            assert!(!link_activation_matches(
                LinkClick {
                    shift: true,
                    ..matching
                },
                activation
            ));
            assert!(link_activation_matches(matching, activation));
        }
    }

    #[test]
    fn opens_only_web_urls() {
        assert_eq!(
            web_link_target("https://google.com").as_deref(),
            Some("https://google.com/")
        );
        assert_eq!(web_link_target("file:///etc/passwd"), None);
        assert_eq!(web_link_target("not a URL"), None);
    }

    #[test]
    fn finds_web_links_like_xterm() {
        assert_eq!(
            web_link_ranges("see https://example.com/a_b?c=d, then"),
            [(4, 31)]
        );
        let line = "(http://host:8080/x).";
        let (start, end) = web_link_ranges(line)[0];
        assert_eq!(&line[start..end], "http://host:8080/x");
        assert!(web_link_ranges("ftp://example.com").is_empty());
        let buffer = StringBuffer::new(12, &[("go https://e", false), ("xample.com/x", true)]);
        let links = web_links_at(&buffer, 1);
        assert_eq!(
            links[0].target,
            LinkTarget::Web("https://example.com/x".into())
        );
        assert_eq!(
            (links[0].start, links[0].end),
            (BufferPoint::new(0, 3), BufferPoint::new(1, 11))
        );
        assert!(link_at(&buffer, BufferPoint::new(1, 5), false).is_some());
    }

    #[test]
    fn normalizes_absolute_and_current_directory_relative_paths() {
        assert_eq!(
            resolve_terminal_file_path(
                "/srv/app/../config.toml",
                Some("/ignored"),
                None,
                PathSource::Posix
            )
            .as_deref(),
            Some("/srv/config.toml")
        );
        assert_eq!(
            resolve_terminal_file_path(
                "./src/../README.md",
                Some("/srv/app"),
                None,
                PathSource::Posix
            )
            .as_deref(),
            Some("/srv/app/README.md")
        );
        assert_eq!(
            resolve_terminal_file_path(
                "../../etc/hosts",
                Some("/srv/app"),
                None,
                PathSource::Posix
            )
            .as_deref(),
            Some("/etc/hosts")
        );
    }

    #[test]
    fn resolves_the_current_user_home_only_when_supplied() {
        assert_eq!(
            resolve_terminal_file_path("~/.ssh/config", Some("/srv/app"), None, PathSource::Posix),
            None
        );
        assert_eq!(
            resolve_terminal_file_path(
                "~/.ssh/config",
                Some("/srv/app"),
                Some("/home/alice"),
                PathSource::Posix
            )
            .as_deref(),
            Some("/home/alice/.ssh/config")
        );
        assert_eq!(
            resolve_terminal_file_path(
                "~bob/.ssh/config",
                Some("/srv/app"),
                Some("/home/alice"),
                PathSource::Posix
            ),
            None
        );
    }

    #[test]
    fn requires_an_absolute_shell_working_directory_for_relative_paths() {
        assert_eq!(
            resolve_terminal_file_path("src/main.ts", None, None, PathSource::Posix),
            None
        );
        assert_eq!(
            resolve_terminal_file_path("src/main.ts", Some("."), None, PathSource::Posix),
            None
        );
    }

    #[test]
    fn normalizes_windows_drive_letter_and_unc_paths_for_local_terminals() {
        assert_eq!(
            resolve_terminal_file_path(
                r"C:\Users\me\..\config.toml",
                None,
                None,
                PathSource::Local
            )
            .as_deref(),
            Some(r"C:\Users\config.toml")
        );
        assert_eq!(
            resolve_terminal_file_path(
                "src/main.ts",
                Some(r"C:\Users\me"),
                None,
                PathSource::Local
            )
            .as_deref(),
            Some(r"C:\Users\me\src\main.ts")
        );
        assert_eq!(
            resolve_terminal_file_path(
                r"\\server\share\dir\..\file.txt",
                None,
                None,
                PathSource::Local
            )
            .as_deref(),
            Some(r"\\server\share\file.txt")
        );
        assert_eq!(
            resolve_terminal_file_path(
                "C:/Users/me/file.txt",
                Some("/srv/app"),
                None,
                PathSource::Posix
            ),
            None
        );
    }
}
