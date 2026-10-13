//! Release feeds: the `latest*.yml` files every release publishes next to its installers
//! (electron-builder's `UpdateInfo`, which electron-updater reads), the file a platform takes
//! from them, and the release pipeline's merge of the two Windows feeds
//! (`hack/update-feeds.mjs`).
//!
//! The feeds are YAML written by electron-builder or by [`to_yaml`]: a small, regular subset
//! (scalars, one list of maps, block scalars for release notes). [`parse_update_info`] reads
//! that subset, and JSON, which is valid YAML and what the updater integration test serves.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// One installer a feed lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFileInfo {
    /// Relative to the feed's folder, or absolute.
    pub url: String,
    /// Base64 SHA-512 of the file.
    pub sha512: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// A release feed (`UpdateInfo` in electron-updater).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInfo {
    pub version: String,
    #[serde(default)]
    pub files: Vec<UpdateFileInfo>,
    /// Legacy single-file feeds name their installer here.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha512: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub release_name: Option<String>,
}

impl UpdateInfo {
    /// The installers to choose from (`getFileList`): `files`, else the legacy `path`.
    pub fn file_list(&self) -> Result<Vec<UpdateFileInfo>, String> {
        if !self.files.is_empty() {
            return Ok(self.files.clone());
        }
        match (&self.path, &self.sha512) {
            (Some(path), Some(sha512)) => Ok(vec![UpdateFileInfo {
                url: path.clone(),
                sha512: sha512.clone(),
                size: None,
            }]),
            _ => Err("No files provided".into()),
        }
    }
}

/// Parse a feed: JSON, or the YAML subset release feeds use.
pub fn parse_update_info(text: &str) -> Result<UpdateInfo, String> {
    let value = parse_feed_value(text)?;
    let mut value = value;
    // js-yaml reads `version: 1.0` as a number; the version is a string either way.
    if let Some(version) = value.get_mut("version")
        && let Value::Number(number) = version
    {
        *version = Value::String(number.to_string());
    }
    serde_json::from_value(value).map_err(|error| format!("Invalid update feed: {error}"))
}

/// A feed document as a JSON value.
pub fn parse_feed_value(text: &str) -> Result<Value, String> {
    let trimmed = text.trim_start_matches('\u{feff}').trim();
    if trimmed.starts_with('{') {
        return serde_json::from_str(trimmed)
            .map_err(|error| format!("Invalid update feed: {error}"));
    }
    parse_yaml_subset(trimmed)
}

/// The feed file name for a platform and CPU (`getChannelFilename` with the `latest` channel):
/// `latest.yml` on Windows, `latest-mac.yml` on macOS, `latest-linux[-<arch>].yml` on Linux.
pub fn channel_file_name(platform: muxus_core::platform::Platform, arch: &str) -> String {
    use muxus_core::platform::Platform;
    match platform {
        Platform::Windows => "latest.yml".into(),
        Platform::MacOs => "latest-mac.yml".into(),
        Platform::Linux | Platform::FreeBsd | Platform::Other => {
            if arch == "x64" {
                "latest-linux.yml".into()
            } else {
                format!("latest-linux-{arch}.yml")
            }
        }
    }
}

/// Node's name for this CPU (`process.arch`), which feeds and file names use.
pub fn node_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "ia32",
        "arm" => "arm",
        other => other,
    }
}

/// The path part of a file URL, for extension checks.
fn url_path(url: &str) -> &str {
    let without_query = url.split(['?', '#']).next().unwrap_or(url);
    match without_query.find("://") {
        Some(scheme) => {
            let rest = &without_query[scheme + 3..];
            rest.find('/').map_or("/", |slash| &rest[slash..])
        }
        None => without_query,
    }
}

/// Pick the installer with `extension` (`findFile`): the one naming this CPU, else the first;
/// without one, the first file not ending in any of `not` (or simply the first file).
pub fn find_file<'a>(
    files: &'a [UpdateFileInfo],
    extension: &str,
    not: Option<&[&str]>,
    arch: &str,
) -> Option<&'a UpdateFileInfo> {
    let suffix = format!(".{}", extension.to_lowercase());
    let matching: Vec<&UpdateFileInfo> = files
        .iter()
        .filter(|file| url_path(&file.url).to_lowercase().ends_with(&suffix))
        .collect();
    if let Some(found) = matching
        .iter()
        .find(|file| url_path(&file.url).contains(arch) || file.url.contains(arch))
        .or_else(|| matching.first())
    {
        return Some(found);
    }
    match not {
        None => files.first(),
        Some(excluded) => files.iter().find(|file| {
            let path = url_path(&file.url).to_lowercase();
            !excluded
                .iter()
                .any(|ext| path.ends_with(&format!(".{}", ext.to_lowercase())))
        }),
    }
}

/// Resolve a feed file URL against the feed's folder (`newUrlFromBase`).
pub fn resolve_file_url(base: &str, url: &str) -> String {
    if url.contains("://") {
        return url.to_string();
    }
    let encoded = url.replace(' ', "%20");
    if let Some(rest) = encoded.strip_prefix('/') {
        // Absolute path on the feed's host.
        let origin_end = base
            .find("://")
            .and_then(|scheme| base[scheme + 3..].find('/').map(|slash| scheme + 3 + slash))
            .unwrap_or(base.len());
        return format!("{}/{rest}", &base[..origin_end]);
    }
    let folder = match base.rfind('/') {
        Some(index) if index > base.find("://").map_or(0, |scheme| scheme + 2) => &base[..=index],
        _ => return format!("{base}/{encoded}"),
    };
    format!("{folder}{encoded}")
}

// --- The YAML subset --------------------------------------------------------------------------

struct Line<'a> {
    indent: usize,
    text: &'a str,
}

fn yaml_lines(text: &str) -> Vec<Line<'_>> {
    text.lines()
        .map(|line| line.trim_end_matches('\r'))
        .filter(|line| {
            let trimmed = line.trim();
            !(trimmed.is_empty() || trimmed.starts_with('#') || trimmed == "---")
        })
        .map(|line| Line {
            indent: line.len() - line.trim_start_matches(' ').len(),
            text: line.trim(),
        })
        .collect()
}

/// A scalar as YAML reads it: quoted strings, integers, booleans and null; everything else
/// is a plain string (versions such as `0.10.0` stay strings).
fn yaml_scalar(raw: &str) -> Result<Value, String> {
    let raw = strip_comment(raw.trim());
    if let Some(inner) = raw.strip_prefix('"') {
        if !inner.ends_with('"') || inner.is_empty() {
            return Err(format!("Unterminated string: {raw}"));
        }
        return serde_json::from_str::<String>(raw)
            .map(Value::String)
            .map_err(|_| format!("Invalid string: {raw}"));
    }
    if let Some(inner) = raw.strip_prefix('\'') {
        let Some(body) = inner.strip_suffix('\'') else {
            return Err(format!("Unterminated string: {raw}"));
        };
        return Ok(Value::String(body.replace("''", "'")));
    }
    match raw {
        "" | "~" | "null" => return Ok(Value::Null),
        "true" => return Ok(Value::Bool(true)),
        "false" => return Ok(Value::Bool(false)),
        _ => {}
    }
    let digits = raw.strip_prefix('-').unwrap_or(raw);
    if !digits.is_empty()
        && digits.bytes().all(|byte| byte.is_ascii_digit())
        && let Ok(number) = raw.parse::<i64>()
    {
        return Ok(Value::from(number));
    }
    Ok(Value::String(raw.to_string()))
}

/// Drop a trailing ` # comment` from a plain scalar.
fn strip_comment(raw: &str) -> &str {
    if raw.starts_with('"') || raw.starts_with('\'') {
        return raw;
    }
    match raw.find(" #") {
        Some(index) => raw[..index].trim_end(),
        None => raw,
    }
}

/// `key: value` (the value may be empty).
fn split_key(text: &str) -> Option<(&str, &str)> {
    let colon = text
        .find(": ")
        .or_else(|| text.strip_suffix(':').map(|t| t.len()))?;
    let key = text[..colon].trim();
    let key = key
        .strip_prefix('"')
        .and_then(|k| k.strip_suffix('"'))
        .or_else(|| key.strip_prefix('\'').and_then(|k| k.strip_suffix('\'')))
        .unwrap_or(key);
    let value = text.get(colon + 1..).unwrap_or("").trim();
    Some((key, value))
}

fn parse_yaml_subset(text: &str) -> Result<Value, String> {
    let lines = yaml_lines(text);
    let mut index = 0;
    let value = parse_map(&lines, &mut index, 0)?;
    if index < lines.len() {
        return Err(format!(
            "Unexpected line in update feed: {}",
            lines[index].text
        ));
    }
    Ok(value)
}

fn parse_map(lines: &[Line<'_>], index: &mut usize, indent: usize) -> Result<Value, String> {
    let mut map = Map::new();
    while let Some(line) = lines.get(*index) {
        if line.indent < indent || line.text.starts_with("- ") || line.text == "-" {
            break;
        }
        if line.indent > indent {
            return Err(format!("Unexpected indentation: {}", line.text));
        }
        let (key, raw) =
            split_key(line.text).ok_or_else(|| format!("Expected `key: value`: {}", line.text))?;
        *index += 1;
        let value = parse_value(lines, index, indent, raw)?;
        map.insert(key.to_string(), value);
    }
    Ok(Value::Object(map))
}

/// The value after `key:` on a line indented by `indent`.
fn parse_value(
    lines: &[Line<'_>],
    index: &mut usize,
    indent: usize,
    raw: &str,
) -> Result<Value, String> {
    if raw == "|" || raw == "|-" || raw == ">" || raw == ">-" || raw == "|+" {
        return Ok(Value::String(block_scalar(lines, index, indent, raw)));
    }
    if !raw.is_empty() {
        return yaml_scalar(raw);
    }
    let Some(next) = lines.get(*index) else {
        return Ok(Value::Null);
    };
    if next.text.starts_with("- ") || next.text == "-" {
        // Sequences may sit at the key's own indentation, as js-yaml writes them.
        if next.indent < indent {
            return Ok(Value::Null);
        }
        return parse_list(lines, index, next.indent);
    }
    if next.indent > indent {
        return parse_map(lines, index, next.indent);
    }
    Ok(Value::Null)
}

fn parse_list(lines: &[Line<'_>], index: &mut usize, indent: usize) -> Result<Value, String> {
    let mut items = Vec::new();
    while let Some(line) = lines.get(*index) {
        if line.indent != indent || !(line.text.starts_with("- ") || line.text == "-") {
            break;
        }
        let rest = line.text[1..].trim_start();
        // A map item: `- key: value`, then more keys indented past the dash.
        if let Some((key, raw)) = split_key(rest).filter(|_| !rest.starts_with(['"', '\''])) {
            let item_indent = indent + (line.text.len() - rest.len());
            *index += 1;
            let mut map = Map::new();
            map.insert(
                key.to_string(),
                parse_value(lines, index, item_indent, raw)?,
            );
            if let Value::Object(more) = parse_map(lines, index, item_indent)? {
                map.extend(more);
            }
            items.push(Value::Object(map));
        } else {
            *index += 1;
            items.push(yaml_scalar(rest)?);
        }
    }
    Ok(Value::Array(items))
}

fn block_scalar(lines: &[Line<'_>], index: &mut usize, indent: usize, style: &str) -> String {
    let mut parts = Vec::new();
    while let Some(line) = lines.get(*index) {
        if line.indent <= indent {
            break;
        }
        parts.push(line.text);
        *index += 1;
    }
    let joined = if style.starts_with('>') {
        parts.join(" ")
    } else {
        parts.join("\n")
    };
    if style.ends_with('-') {
        joined
    } else {
        format!("{joined}\n")
    }
}

// --- Merging the Windows feeds (`hack/update-feeds.mjs`) ----------------------------------------

/// The Windows installers built in separate jobs, each with its own feed.
pub const WINDOWS_ARCHES: [&str; 2] = ["x64", "arm64"];

fn validate_feed<'a>(
    feed: Option<&'a Value>,
    arch: &str,
    version: Option<&str>,
) -> Result<&'a Map<String, Value>, String> {
    let feed_version = feed
        .and_then(|feed| feed.get("version"))
        .and_then(Value::as_str)
        .ok_or_else(|| format!("{arch} feed has no version"))?;
    if let Some(version) = version
        && feed_version != version
    {
        return Err(format!("{arch} feed is for {feed_version}, not {version}"));
    }
    let files = feed
        .and_then(|feed| feed.get("files"))
        .and_then(Value::as_array)
        .filter(|files| files.len() == 1)
        .ok_or_else(|| format!("{arch} feed must list exactly one installer"))?;
    let file = files[0]
        .as_object()
        .ok_or_else(|| format!("{arch} feed must list exactly one installer"))?;
    let url = file.get("url").and_then(Value::as_str).unwrap_or_default();
    if !url.ends_with(&format!("-win-{arch}.exe")) {
        return Err(format!("{arch} feed lists {url}"));
    }
    if !file.get("sha512").is_some_and(Value::is_string) {
        return Err(format!("{arch} installer has no checksum"));
    }
    const MAX_SAFE_INTEGER: u64 = 9_007_199_254_740_991;
    if !file
        .get("size")
        .and_then(Value::as_u64)
        .is_some_and(|size| size > 0 && size <= MAX_SAFE_INTEGER)
    {
        return Err(format!("{arch} installer has no size"));
    }
    Ok(file)
}

/// Combine per-architecture feeds; x64 stays the legacy top-level default.
pub fn merge_windows_feeds(feeds: &BTreeMap<&str, Value>, version: &str) -> Result<Value, String> {
    let mut files = Vec::new();
    for arch in WINDOWS_ARCHES {
        files.push(validate_feed(feeds.get(arch), arch, Some(version))?);
    }
    let mut release_dates: Vec<&str> = WINDOWS_ARCHES
        .iter()
        .filter_map(|arch| feeds.get(arch)?.get("releaseDate")?.as_str())
        .filter(|date| !date.is_empty())
        .collect();
    // ISO timestamps: `localeCompare` and byte order agree.
    release_dates.sort_unstable();
    let mut merged = Map::new();
    merged.insert("version".into(), feeds["x64"]["version"].clone());
    merged.insert(
        "files".into(),
        Value::Array(
            files
                .iter()
                .map(|file| {
                    let mut entry = Map::new();
                    for key in ["url", "sha512", "size"] {
                        entry.insert(key.into(), file[key].clone());
                    }
                    Value::Object(entry)
                })
                .collect(),
        ),
    );
    merged.insert("path".into(), files[0]["url"].clone());
    merged.insert("sha512".into(), files[0]["sha512"].clone());
    if let Some(latest) = release_dates.last() {
        merged.insert("releaseDate".into(), Value::String((*latest).to_string()));
    }
    Ok(Value::Object(merged))
}

/// YAML with JSON-quoted scalars: unambiguous for any YAML parser.
pub fn to_yaml(feed: &Value) -> String {
    let scalar = |value: &Value| match value {
        Value::Number(number) => number.to_string(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    let mut lines = vec![
        format!("version: {}", scalar(&feed["version"])),
        "files:".into(),
    ];
    for file in feed["files"].as_array().into_iter().flatten() {
        lines.push(format!("  - url: {}", scalar(&file["url"])));
        lines.push(format!("    sha512: {}", scalar(&file["sha512"])));
        lines.push(format!("    size: {}", scalar(&file["size"])));
    }
    lines.push(format!("path: {}", scalar(&feed["path"])));
    lines.push(format!("sha512: {}", scalar(&feed["sha512"])));
    if let Some(date) = feed.get("releaseDate").filter(|date| !date.is_null()) {
        lines.push(format!("releaseDate: {}", scalar(date)));
    }
    format!("{}\n", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The feed electron-builder wrote for the last Linux release.
    const ELECTRON_BUILDER_FEED: &str = "version: 0.10.0
files:
  - url: muxus-0.10.0-linux-x86_64.AppImage
    sha512: Uy9jseKwPHKraCGwxa7LLXRT6vLHTv+aqae75Ko4TEHKw+jt1778YeSVtlLkJ2+obEZ+6+6Uz6LQqFI6s1Xv9w==
    size: 111580584
    blockMapSize: 117260
  - url: muxus-0.10.0-linux-amd64.deb
    sha512: c2hhNTEy
    size: 111580585
path: muxus-0.10.0-linux-x86_64.AppImage
sha512: Uy9jseKwPHKraCGwxa7LLXRT6vLHTv+aqae75Ko4TEHKw+jt1778YeSVtlLkJ2+obEZ+6+6Uz6LQqFI6s1Xv9w==
releaseDate: '2026-10-08T14:36:52.046Z'
releaseNotes: |
  Fixes:
  - one
";

    #[test]
    fn reads_electron_builder_feeds() {
        let info = parse_update_info(ELECTRON_BUILDER_FEED).unwrap();
        assert_eq!(info.version, "0.10.0");
        assert_eq!(info.files.len(), 2);
        assert_eq!(info.files[0].url, "muxus-0.10.0-linux-x86_64.AppImage");
        assert_eq!(info.files[0].size, Some(111_580_584));
        assert!(info.files[0].sha512.ends_with("Xv9w=="));
        assert_eq!(
            info.release_date.as_deref(),
            Some("2026-10-08T14:36:52.046Z")
        );
        let value = parse_feed_value(ELECTRON_BUILDER_FEED).unwrap();
        assert_eq!(value["releaseNotes"], json!("Fixes:\n- one\n"));
    }

    #[test]
    fn reads_json_feeds_and_numeric_versions() {
        let info = parse_update_info(
            r#"{"version":"1.2.3","files":[{"url":"update.AppImage","sha512":"abc","size":3}],"path":"update.AppImage","sha512":"abc"}"#,
        )
        .unwrap();
        assert_eq!(info.files[0].url, "update.AppImage");
        assert_eq!(
            parse_update_info("version: 2\npath: a\nsha512: b\n")
                .unwrap()
                .version,
            "2"
        );
        assert!(parse_update_info("files:\n  - url: a\n").is_err());
    }

    #[test]
    fn falls_back_to_the_legacy_path() {
        let info = parse_update_info("version: 1.0.0\npath: Muxus.exe\nsha512: abc\n").unwrap();
        assert_eq!(
            info.file_list().unwrap(),
            vec![UpdateFileInfo {
                url: "Muxus.exe".into(),
                sha512: "abc".into(),
                size: None
            }]
        );
        let empty = parse_update_info("version: 1.0.0\n").unwrap();
        assert!(empty.file_list().is_err());
    }

    #[test]
    fn picks_the_installer_for_this_platform_and_cpu() {
        let file = |url: &str| UpdateFileInfo {
            url: url.into(),
            sha512: "x".into(),
            size: Some(1),
        };
        let linux = [
            file("muxus-1.0.0-linux-amd64.deb"),
            file("muxus-1.0.0-linux-x86_64.AppImage"),
        ];
        assert_eq!(
            find_file(&linux, "AppImage", Some(&["rpm", "deb", "pacman"]), "x64")
                .unwrap()
                .url,
            "muxus-1.0.0-linux-x86_64.AppImage"
        );
        let windows = [
            file("muxus-1.0.0-win-x64.exe"),
            file("muxus-1.0.0-win-arm64.exe"),
        ];
        assert_eq!(
            find_file(&windows, "exe", None, "arm64").unwrap().url,
            "muxus-1.0.0-win-arm64.exe"
        );
        assert_eq!(
            find_file(&windows, "exe", None, "x64").unwrap().url,
            "muxus-1.0.0-win-x64.exe"
        );
        // Nothing with the extension: the first file that is not excluded.
        let debs = [file("a.deb"), file("b.tar.gz")];
        assert_eq!(
            find_file(&debs, "AppImage", Some(&["deb"]), "x64")
                .unwrap()
                .url,
            "b.tar.gz"
        );
        assert_eq!(
            find_file(&debs, "AppImage", None, "x64").unwrap().url,
            "a.deb"
        );
    }

    #[test]
    fn names_the_feed_of_each_platform() {
        use muxus_core::platform::Platform;
        assert_eq!(
            channel_file_name(Platform::Linux, "x64"),
            "latest-linux.yml"
        );
        assert_eq!(
            channel_file_name(Platform::Linux, "arm64"),
            "latest-linux-arm64.yml"
        );
        assert_eq!(channel_file_name(Platform::Windows, "arm64"), "latest.yml");
        assert_eq!(
            channel_file_name(Platform::MacOs, "arm64"),
            "latest-mac.yml"
        );
    }

    #[test]
    fn resolves_file_urls_against_the_feed_folder() {
        let base = "https://github.com/FloSch62/muxus/releases/download/v1.0.0/latest-linux.yml";
        assert_eq!(
            resolve_file_url(base, "Muxus 1.0.0.AppImage"),
            "https://github.com/FloSch62/muxus/releases/download/v1.0.0/Muxus%201.0.0.AppImage"
        );
        assert_eq!(
            resolve_file_url(base, "https://cdn.example/x"),
            "https://cdn.example/x"
        );
        assert_eq!(
            resolve_file_url("http://127.0.0.1:9/feed/latest.yml", "/top.exe"),
            "http://127.0.0.1:9/top.exe"
        );
    }

    // `hack/update-feeds.test.mjs`

    fn windows_feed(arch: &str, overrides: Value) -> Value {
        let mut feed = json!({
            "version": "0.8.0",
            "files": [{
                "url": format!("muxus-0.8.0-win-{arch}.exe"),
                "sha512": format!("{arch}+/sha=="),
                "size": if arch == "x64" { 101 } else { 202 },
            }],
            "path": format!("muxus-0.8.0-win-{arch}.exe"),
            "sha512": format!("{arch}+/sha=="),
            "releaseDate": if arch == "x64" { "2026-10-02T10:00:00.000Z" } else { "2026-10-02T10:05:00.000Z" },
        });
        if let (Value::Object(feed), Value::Object(overrides)) = (&mut feed, overrides) {
            feed.extend(overrides);
        }
        feed
    }

    fn feeds(x64: Option<Value>, arm64: Option<Value>) -> BTreeMap<&'static str, Value> {
        let mut map = BTreeMap::new();
        if let Some(feed) = x64 {
            map.insert("x64", feed);
        }
        if let Some(feed) = arm64 {
            map.insert("arm64", feed);
        }
        map
    }

    #[test]
    fn lists_both_windows_installers_so_the_updater_can_choose_by_architecture() {
        let merged = merge_windows_feeds(
            &feeds(
                Some(windows_feed("x64", json!({}))),
                Some(windows_feed("arm64", json!({}))),
            ),
            "0.8.0",
        )
        .unwrap();
        let urls: Vec<&str> = merged["files"]
            .as_array()
            .unwrap()
            .iter()
            .map(|file| file["url"].as_str().unwrap())
            .collect();
        assert_eq!(
            urls,
            ["muxus-0.8.0-win-x64.exe", "muxus-0.8.0-win-arm64.exe"]
        );
        // x64 stays the default for clients that read only path.
        assert_eq!(merged["path"], "muxus-0.8.0-win-x64.exe");
        assert_eq!(merged["releaseDate"], "2026-10-02T10:05:00.000Z");
    }

    #[test]
    fn writes_yaml_that_reads_back_to_the_merged_feed() {
        let merged = merge_windows_feeds(
            &feeds(
                Some(windows_feed("x64", json!({}))),
                Some(windows_feed("arm64", json!({}))),
            ),
            "0.8.0",
        )
        .unwrap();
        assert_eq!(parse_feed_value(&to_yaml(&merged)).unwrap(), merged);
        let info = parse_update_info(&to_yaml(&merged)).unwrap();
        assert_eq!(info.files[1].size, Some(202));
    }

    #[test]
    fn refuses_feeds_from_another_release_or_with_the_wrong_installer() {
        let other = merge_windows_feeds(
            &feeds(
                Some(windows_feed("x64", json!({}))),
                Some(windows_feed("arm64", json!({"version": "0.7.0"}))),
            ),
            "0.8.0",
        );
        assert!(other.unwrap_err().contains("not 0.8.0"));
        let wrong = merge_windows_feeds(
            &feeds(
                Some(windows_feed("arm64", json!({}))),
                Some(windows_feed("arm64", json!({}))),
            ),
            "0.8.0",
        );
        assert!(wrong.unwrap_err().contains("x64 feed lists"));
        let missing =
            merge_windows_feeds(&feeds(Some(windows_feed("x64", json!({}))), None), "0.8.0");
        assert!(missing.unwrap_err().contains("arm64 feed has no version"));
        let mut two_files = windows_feed("x64", json!({}));
        let first = two_files["files"][0].clone();
        two_files["files"].as_array_mut().unwrap().push(first);
        let doubled = merge_windows_feeds(
            &feeds(Some(two_files), Some(windows_feed("arm64", json!({})))),
            "0.8.0",
        );
        assert!(doubled.unwrap_err().contains("exactly one installer"));
    }
}
