//! The "Open with" list on Linux (`electron/src/linux-applications.ts`), read
//! straight from the freedesktop.org data files every desktop uses: Desktop
//! Entry files for the programs, shared-mime-info for file types, and
//! mimeapps.list for associations.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use regex::Regex;

const MAX_DESKTOP_FILE_BYTES: u64 = 256 * 1024;
const MAX_APPLICATION_DIRECTORY_DEPTH: usize = 3;

/// One program a file can be opened with.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DesktopApplication {
    /// Desktop file ID, e.g. `org.gnome.TextEditor.desktop`.
    pub id: String,
    /// Absolute path of the .desktop file, for the `%k` field code.
    pub file: String,
    pub name: String,
    pub exec: String,
    pub icon: Option<String>,
    pub working_directory: Option<String>,
    /// A program that must exist for the entry to count as installed.
    pub try_exec: Option<String>,
    /// NoDisplay: offered only where an association names it explicitly.
    pub hidden: bool,
    pub mime_types: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApplicationChoice {
    pub application: DesktopApplication,
    pub recommended: bool,
    pub is_default: bool,
}

pub struct MimeGlob {
    pub weight: f64,
    pub mime_type: String,
    pub pattern: String,
    matcher: GlobMatcher,
}

enum GlobMatcher {
    Exact(String, bool),
    Suffix(String, bool),
    Pattern(Option<Regex>, bool),
}

impl MimeGlob {
    pub fn matches(&self, name: &str) -> bool {
        let normalize = |value: &str, case_sensitive: bool| {
            if case_sensitive {
                value.to_string()
            } else {
                value.to_lowercase()
            }
        };
        match &self.matcher {
            GlobMatcher::Exact(wanted, cs) => normalize(name, *cs) == *wanted,
            GlobMatcher::Suffix(suffix, cs) => normalize(name, *cs).ends_with(suffix.as_str()),
            GlobMatcher::Pattern(Some(expression), cs) => {
                expression.is_match(&normalize(name, *cs))
            }
            GlobMatcher::Pattern(None, _) => false,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct MimeAppsList {
    pub defaults: Vec<(String, Vec<String>)>,
    pub added: Vec<(String, Vec<String>)>,
    pub removed: Vec<(String, Vec<String>)>,
}

/// `de_DE.UTF-8@euro` → the Desktop Entry locale keys to try, most specific first.
pub fn desktop_locale_keys(locale: Option<&str>) -> Vec<String> {
    let Some(locale) = locale.filter(|l| !l.is_empty()) else {
        return Vec::new();
    };
    let c_or_posix = Regex::new(r"^(C|POSIX)([.@]|$)").expect("valid");
    if c_or_posix.is_match(locale) {
        return Vec::new();
    }
    let pattern =
        Regex::new(r"^([A-Za-z]+)(?:_([A-Za-z0-9]+))?(?:\.[^@]*)?(?:@(.+))?$").expect("valid");
    let Some(captures) = pattern.captures(locale) else {
        return Vec::new();
    };
    let language = &captures[1];
    let country = captures.get(2).map(|m| m.as_str());
    let modifier = captures.get(3).map(|m| m.as_str());
    let mut keys = Vec::new();
    if let (Some(country), Some(modifier)) = (country, modifier) {
        keys.push(format!("{language}_{country}@{modifier}"));
    }
    if let Some(country) = country {
        keys.push(format!("{language}_{country}"));
    }
    if let Some(modifier) = modifier {
        keys.push(format!("{language}@{modifier}"));
    }
    keys.push(language.to_string());
    keys
}

type KeyGroup = Vec<(String, String)>;

/// Every group of a freedesktop key file; localized keys are kept as `Name[de]`.
pub fn parse_key_file(text: &str) -> Vec<(String, KeyGroup)> {
    let mut groups: Vec<(String, KeyGroup)> = Vec::new();
    let mut current: Option<usize> = None;
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r').trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            let group = &line[1..line.len() - 1];
            // A repeated group is a malformed file; the first one wins.
            if groups.iter().any(|(name, _)| name == group) {
                current = None;
            } else {
                groups.push((group.to_string(), Vec::new()));
                current = Some(groups.len() - 1);
            }
            continue;
        }
        let Some(index) = current else { continue };
        let Some(equals) = line.find('=').filter(|e| *e > 0) else {
            continue;
        };
        let key = line[..equals].trim().to_string();
        let values = &mut groups[index].1;
        if !values.iter().any(|(k, _)| *k == key) {
            values.push((key, line[equals + 1..].trim().to_string()));
        }
    }
    groups
}

fn group<'a>(groups: &'a [(String, KeyGroup)], name: &str) -> Option<&'a KeyGroup> {
    groups.iter().find(|(n, _)| n == name).map(|(_, g)| g)
}

fn get<'a>(values: &'a KeyGroup, key: &str) -> Option<&'a str> {
    values
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
}

/// Undo the escapes a Desktop Entry `string` value may contain.
pub fn unescape_desktop_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\'
            && let Some(&next) = chars.peek()
            && "sntr\\".contains(next)
        {
            chars.next();
            out.push(match next {
                's' => ' ',
                'n' => '\n',
                't' => '\t',
                'r' => '\r',
                _ => '\\',
            });
        } else {
            out.push(c);
        }
    }
    out
}

/// Split a `;`-separated Desktop Entry list, honouring `\;`.
pub fn desktop_list(value: Option<&str>) -> Vec<String> {
    let Some(value) = value.filter(|v| !v.is_empty()) else {
        return Vec::new();
    };
    let mut items = Vec::new();
    let mut current = String::new();
    let chars: Vec<char> = value.chars().collect();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c == '\\' && chars.get(index + 1) == Some(&';') {
            current.push(';');
            index += 1;
        } else if c == ';' {
            items.push(std::mem::take(&mut current));
        } else {
            current.push(c);
        }
        index += 1;
    }
    items.push(current);
    items
        .into_iter()
        .map(|item| unescape_desktop_string(&item).trim().to_string())
        .filter(|item| !item.is_empty())
        .collect()
}

fn localized<'a>(values: &'a KeyGroup, key: &str, locale_keys: &[String]) -> Option<&'a str> {
    for locale in locale_keys {
        if let Some(value) = get(values, &format!("{key}[{locale}]")).filter(|v| !v.is_empty()) {
            return Some(value);
        }
    }
    get(values, key)
}

/// A program the chooser may offer, or `None` for entries that cannot open a
/// file: links, deleted entries, terminal programs, and entries meant for
/// other desktops. NoDisplay entries are kept but marked hidden, since
/// mimeapps.list may still name one as the default.
pub fn desktop_application_from_text(
    text: &str,
    id: &str,
    file: &str,
    locale_keys: &[String],
    current_desktops: &[String],
) -> Option<DesktopApplication> {
    let groups = parse_key_file(text);
    let empty = Vec::new();
    let values = group(&groups, "Desktop Entry").unwrap_or(&empty);
    if get(values, "Type") != Some("Application") || get(values, "Hidden") == Some("true") {
        return None;
    }
    // Muxus cannot know which terminal emulator to wrap these in.
    if get(values, "Terminal") == Some("true") {
        return None;
    }
    let only_show_in = desktop_list(get(values, "OnlyShowIn"));
    if !only_show_in.is_empty() && !only_show_in.iter().any(|d| current_desktops.contains(d)) {
        return None;
    }
    if desktop_list(get(values, "NotShowIn"))
        .iter()
        .any(|d| current_desktops.contains(d))
    {
        return None;
    }
    let exec = unescape_desktop_string(get(values, "Exec").unwrap_or(""));
    let name = unescape_desktop_string(localized(values, "Name", locale_keys).unwrap_or(""));
    if exec.is_empty() || name.is_empty() {
        return None;
    }
    let optional = |value: Option<&str>| {
        let value = unescape_desktop_string(value.unwrap_or(""));
        (!value.is_empty()).then_some(value)
    };
    Some(DesktopApplication {
        id: id.into(),
        file: file.into(),
        name,
        exec,
        icon: optional(localized(values, "Icon", locale_keys)),
        working_directory: optional(get(values, "Path")),
        try_exec: optional(get(values, "TryExec")),
        hidden: get(values, "NoDisplay") == Some("true"),
        mime_types: desktop_list(get(values, "MimeType")),
    })
}

/// Split an (already unescaped) Exec value into arguments; `None` when it is malformed.
pub fn split_exec(exec: &str) -> Option<Vec<String>> {
    let chars: Vec<char> = exec.chars().collect();
    let mut args = Vec::new();
    let mut current = String::new();
    let mut in_argument = false;
    let mut quoted = false;
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if quoted {
            let next = chars.get(index + 1).copied();
            if c == '\\' && next.is_some_and(|n| "\"`$\\".contains(n)) {
                current.push(next.expect("checked"));
                index += 1;
            } else if c == '"' {
                quoted = false;
            } else {
                current.push(c);
            }
            index += 1;
            continue;
        }
        if c == '"' {
            quoted = true;
            in_argument = true;
        } else if c == ' ' || c == '\t' || c == '\n' {
            if in_argument {
                args.push(std::mem::take(&mut current));
            }
            current.clear();
            in_argument = false;
        } else if c == '\\' && index + 1 < chars.len() {
            index += 1;
            current.push(chars[index]);
            in_argument = true;
        } else {
            current.push(c);
            in_argument = true;
        }
        index += 1;
    }
    if quoted {
        return None;
    }
    if in_argument {
        args.push(current);
    }
    (!args.is_empty()).then_some(args)
}

/// Node's `pathToFileURL(path).href` for an absolute POSIX path.
pub fn file_url(path: &str) -> String {
    let mut url = String::from("file://");
    for byte in path.as_bytes() {
        let c = *byte as char;
        if byte.is_ascii_alphanumeric() || "-._~/!$&'()*+,;=:@".contains(c) {
            url.push(c);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

/// The argument vector that opens `target` with `application`, following the
/// Exec field codes. Programs that take no file argument get it appended, as
/// an explicit "Open with" asks them to open this file.
pub fn expand_exec(application: &DesktopApplication, target: &str) -> Option<Vec<String>> {
    let args = split_exec(&application.exec)?;
    let uri = file_url(target);
    let mut target_used = false;
    let mut expanded = Vec::new();
    for arg in args {
        match arg.as_str() {
            "%f" | "%F" => {
                expanded.push(target.to_string());
                target_used = true;
                continue;
            }
            "%u" | "%U" => {
                expanded.push(uri.clone());
                target_used = true;
                continue;
            }
            "%i" => {
                if let Some(icon) = &application.icon {
                    expanded.push("--icon".into());
                    expanded.push(icon.clone());
                }
                continue;
            }
            // Deprecated codes expand to nothing; a lone one leaves no argument.
            "%d" | "%D" | "%n" | "%N" | "%v" | "%m" => continue,
            _ => {}
        }
        let chars: Vec<char> = arg.chars().collect();
        let mut value = String::new();
        let mut index = 0;
        while index < chars.len() {
            let c = chars[index];
            let code = chars.get(index + 1).copied();
            if c != '%' || code.is_none() {
                value.push(c);
                index += 1;
                continue;
            }
            let code = code.expect("checked");
            index += 2;
            match code {
                '%' => value.push('%'),
                'f' | 'F' => {
                    value.push_str(target);
                    target_used = true;
                }
                'u' | 'U' => {
                    value.push_str(&uri);
                    target_used = true;
                }
                'c' => value.push_str(&application.name),
                'k' => value.push_str(&application.file),
                code if "idDnNvm".contains(code) => {}
                code => {
                    value.push('%');
                    value.push(code);
                }
            }
        }
        expanded.push(value);
    }
    if !target_used {
        expanded.push(target.to_string());
    }
    expanded
        .first()
        .is_some_and(|first| !first.is_empty())
        .then_some(expanded)
}

fn glob_matcher(pattern: &str, case_sensitive: bool) -> GlobMatcher {
    let wanted = if case_sensitive {
        pattern.to_string()
    } else {
        pattern.to_lowercase()
    };
    if !pattern.contains(['*', '?', '[']) {
        return GlobMatcher::Exact(wanted, case_sensitive);
    }
    if let Some(rest) = pattern.strip_prefix('*')
        && !rest.is_empty()
        && !rest.contains(['*', '?', '['])
    {
        return GlobMatcher::Suffix(wanted[1..].to_string(), case_sensitive);
    }
    let chars: Vec<char> = wanted.chars().collect();
    let mut source = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        match c {
            '*' => source.push_str(".*"),
            '?' => source.push('.'),
            '[' => {
                let close = (index + 2..chars.len()).find(|i| chars[*i] == ']');
                match close {
                    None => source.push_str("\\["),
                    Some(close) => {
                        let body: String = chars[index + 1..close].iter().collect();
                        let body = body.replace('\\', "\\\\");
                        match body.strip_prefix('!') {
                            Some(negated) => source.push_str(&format!("[^{negated}]")),
                            None => source.push_str(&format!("[{body}]")),
                        }
                        index = close;
                    }
                }
            }
            c if ".+^${}()|\\]".contains(c) => {
                source.push('\\');
                source.push(c);
            }
            c => source.push(c),
        }
        index += 1;
    }
    GlobMatcher::Pattern(Regex::new(&format!("(?s)^{source}$")).ok(), case_sensitive)
}

/// shared-mime-info `globs2`: `weight:type:pattern[:flags]` per line.
pub fn parse_mime_globs(text: &str) -> Vec<MimeGlob> {
    let mut globs = Vec::new();
    for line in text.split('\n').map(|l| l.trim_end_matches('\r')) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let mut parts = line.split(':');
        let (Some(weight), Some(mime_type), Some(pattern)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        let flags = parts.next().unwrap_or("");
        let Ok(weight) = weight.trim().parse::<f64>() else {
            continue;
        };
        if mime_type.is_empty() || pattern.is_empty() || pattern == "__NOGLOBS__" {
            continue;
        }
        let case_sensitive = flags.split(',').any(|flag| flag == "cs");
        globs.push(MimeGlob {
            weight,
            mime_type: mime_type.into(),
            pattern: pattern.into(),
            matcher: glob_matcher(pattern, case_sensitive),
        });
    }
    globs
}

/// The file type a name implies: highest weight first, then the most specific pattern.
pub fn mime_type_for_file_name(name: &str, globs: &[MimeGlob]) -> Option<String> {
    let mut best: Option<&MimeGlob> = None;
    for glob in globs.iter().filter(|glob| glob.matches(name)) {
        let better = best.is_none_or(|best| {
            glob.weight > best.weight
                || (glob.weight == best.weight && glob.pattern.len() > best.pattern.len())
        });
        if better {
            best = Some(glob);
        }
    }
    best.map(|glob| glob.mime_type.clone())
}

/// shared-mime-info `subclasses` (`child parent`) or `aliases` (`alias type`) lines.
pub fn parse_mime_pairs(text: &str) -> HashMap<String, Vec<String>> {
    let mut pairs: HashMap<String, Vec<String>> = HashMap::new();
    for line in text.split('\n') {
        let mut words = line.split_whitespace();
        let (Some(left), Some(right)) = (words.next(), words.next()) else {
            continue;
        };
        if left.starts_with('#') {
            continue;
        }
        pairs.entry(left.into()).or_default().push(right.into());
    }
    pairs
}

/// Broader types whose programs also handle `mime_type`, nearest first.
pub fn mime_type_ancestors(
    mime_type: &str,
    subclasses: &HashMap<String, Vec<String>>,
) -> Vec<String> {
    let mut ancestors: Vec<String> = Vec::new();
    let mut queue = std::collections::VecDeque::from([mime_type.to_string()]);
    while let Some(current) = queue.pop_front() {
        let mut parents = subclasses.get(&current).cloned().unwrap_or_default();
        if current.starts_with("text/") && current != "text/plain" {
            parents.push("text/plain".into());
        }
        for parent in parents {
            if parent == mime_type || ancestors.contains(&parent) {
                continue;
            }
            ancestors.push(parent.clone());
            queue.push_back(parent);
        }
    }
    ancestors
}

/// One `mimeapps.list` (or legacy `defaults.list`) file.
pub fn parse_mime_apps_list(text: &str) -> MimeAppsList {
    let groups = parse_key_file(text);
    let read = |name: &str| -> Vec<(String, Vec<String>)> {
        group(&groups, name)
            .map(|values| {
                values
                    .iter()
                    .map(|(mime, value)| (mime.clone(), desktop_list(Some(value))))
                    .collect()
            })
            .unwrap_or_default()
    };
    MimeAppsList {
        defaults: read("Default Applications"),
        added: read("Added Associations"),
        removed: read("Removed Associations"),
    }
}

/// Order every program for a file of `mime_type`: the desktop's default
/// first, then programs that declare the type (or a broader one), then the rest.
pub fn rank_applications(
    applications: &[DesktopApplication],
    mime_type: Option<&str>,
    subclasses: &HashMap<String, Vec<String>>,
    aliases: &HashMap<String, Vec<String>>,
    mime_apps: &[MimeAppsList],
) -> Vec<ApplicationChoice> {
    let canonical = |kind: &str| -> String {
        aliases
            .get(kind)
            .and_then(|targets| targets.first())
            .cloned()
            .unwrap_or_else(|| kind.to_string())
    };
    let known: HashSet<&str> = applications.iter().map(|a| a.id.as_str()).collect();
    let types: Vec<String> = match mime_type {
        Some(kind) => {
            let kind = canonical(kind);
            let mut types = vec![kind.clone()];
            types.extend(mime_type_ancestors(&kind, subclasses));
            types
        }
        None => Vec::new(),
    };
    let mut rank_by_id: HashMap<String, usize> = HashMap::new();
    let mut associated: HashSet<String> = HashSet::new();
    let mut default_id: Option<String> = None;
    for (rank, kind) in types.iter().enumerate() {
        let mut removed: HashSet<String> = HashSet::new();
        for list in mime_apps {
            for (listed, ids) in &list.removed {
                if canonical(listed) == *kind {
                    removed.extend(ids.iter().cloned());
                }
            }
        }
        let mut consider = |id: &str, explicit: bool| {
            if !known.contains(id) || removed.contains(id) {
                return;
            }
            if explicit {
                associated.insert(id.to_string());
            }
            rank_by_id.entry(id.to_string()).or_insert(rank);
        };
        for list in mime_apps {
            for (listed, ids) in &list.defaults {
                if canonical(listed) != *kind {
                    continue;
                }
                if rank == 0 && default_id.is_none() {
                    default_id = ids.iter().find(|id| known.contains(id.as_str())).cloned();
                }
                for id in ids {
                    consider(id, true);
                }
            }
            for (listed, ids) in &list.added {
                if canonical(listed) == *kind {
                    for id in ids {
                        consider(id, true);
                    }
                }
            }
        }
        for application in applications {
            if application
                .mime_types
                .iter()
                .any(|declared| canonical(declared) == *kind)
            {
                consider(&application.id, false);
            }
        }
    }
    let mut choices: Vec<ApplicationChoice> = applications
        .iter()
        .filter(|application| !application.hidden || associated.contains(&application.id))
        .map(|application| ApplicationChoice {
            application: application.clone(),
            recommended: rank_by_id.contains_key(&application.id),
            is_default: Some(&application.id) == default_id.as_ref(),
        })
        .collect();
    choices.sort_by(|left, right| {
        right
            .is_default
            .cmp(&left.is_default)
            .then(right.recommended.cmp(&left.recommended))
            .then_with(|| {
                if left.recommended {
                    rank_by_id[&left.application.id].cmp(&rank_by_id[&right.application.id])
                } else {
                    std::cmp::Ordering::Equal
                }
            })
            .then_with(|| {
                muxus_core::collate::locale_compare(
                    &left.application.name.to_lowercase(),
                    &right.application.name.to_lowercase(),
                )
            })
    });
    choices
}

fn read_text(file: &Path, max_bytes: u64) -> Option<String> {
    let info = std::fs::metadata(file).ok()?;
    if !info.is_file() || info.len() > max_bytes {
        return None;
    }
    std::fs::read_to_string(file).ok()
}

fn is_executable(file: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(file)
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        file.is_file()
    }
}

/// Where a program name resolves on PATH, as the launcher would find it.
pub fn find_program(program: &str, path_var: &str) -> Option<PathBuf> {
    if program.contains('/') {
        let path = Path::new(program);
        return (path.is_absolute() && is_executable(path)).then(|| path.to_path_buf());
    }
    path_var
        .split(':')
        .filter(|dir| Path::new(dir).is_absolute())
        .map(|dir| Path::new(dir).join(program))
        .find(|candidate| is_executable(candidate))
}

fn absolute_paths(value: Option<&str>) -> Vec<PathBuf> {
    value
        .unwrap_or("")
        .split(':')
        .filter(|entry| Path::new(entry).is_absolute())
        .map(PathBuf::from)
        .collect()
}

fn unique(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// XDG data directories, most important first.
pub fn data_directories(env: &HashMap<String, String>, home: &Path) -> Vec<PathBuf> {
    let data_home = env
        .get("XDG_DATA_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".local/share"));
    let mut dirs = absolute_paths(env.get("XDG_DATA_DIRS").map(String::as_str));
    if dirs.is_empty() {
        dirs = vec!["/usr/local/share".into(), "/usr/share".into()];
    }
    unique(std::iter::once(data_home).chain(dirs).collect())
}

fn config_directories(env: &HashMap<String, String>, home: &Path) -> Vec<PathBuf> {
    let config_home = env
        .get("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .unwrap_or_else(|| home.join(".config"));
    let mut dirs = absolute_paths(env.get("XDG_CONFIG_DIRS").map(String::as_str));
    if dirs.is_empty() {
        dirs = vec!["/etc/xdg".into()];
    }
    unique(std::iter::once(config_home).chain(dirs).collect())
}

/// Desktops named by XDG_CURRENT_DESKTOP, as OnlyShowIn/NotShowIn compare them.
pub fn current_desktops(env: &HashMap<String, String>) -> Vec<String> {
    env.get("ORIGINAL_XDG_CURRENT_DESKTOP")
        .filter(|v| !v.is_empty())
        .or_else(|| env.get("XDG_CURRENT_DESKTOP"))
        .map(|value| {
            value
                .split(':')
                .filter(|d| !d.is_empty())
                .map(String::from)
                .collect()
        })
        .unwrap_or_default()
}

fn desktop_files(directory: &Path, prefix: &str, depth: usize, found: &mut Vec<(String, PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut entries: Vec<_> = entries.flatten().collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name().to_string_lossy().into_owned();
        let file = entry.path();
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() {
            if depth < MAX_APPLICATION_DIRECTORY_DEPTH {
                desktop_files(&file, &format!("{prefix}{name}-"), depth + 1, found);
            }
        } else if (kind.is_file() || kind.is_symlink()) && name.ends_with(".desktop") {
            let id = format!("{prefix}{name}");
            // An earlier data directory shadows the same ID later on, even when
            // that earlier entry is Hidden — that is how users delete an entry.
            if !found.iter().any(|(existing, _)| *existing == id) {
                found.push((id, file));
            }
        }
    }
}

/// The installed programs and file-type data, read in one go.
pub struct LinuxApplicationCatalog {
    pub applications: Vec<DesktopApplication>,
    globs: Vec<MimeGlob>,
    subclasses: HashMap<String, Vec<String>>,
    aliases: HashMap<String, Vec<String>>,
    mime_apps: Vec<MimeAppsList>,
}

impl LinuxApplicationCatalog {
    pub fn read(env: &HashMap<String, String>, home: &Path) -> Self {
        let data_dirs = data_directories(env, home);
        let desktops = current_desktops(env);
        let locale = env
            .get("LC_ALL")
            .filter(|v| !v.is_empty())
            .or_else(|| env.get("LC_MESSAGES").filter(|v| !v.is_empty()))
            .or_else(|| env.get("LANG"))
            .map(String::as_str);
        let locale_keys = desktop_locale_keys(locale);
        let path_var = env.get("PATH").cloned().unwrap_or_default();

        let mut files = Vec::new();
        for directory in &data_dirs {
            desktop_files(&directory.join("applications"), "", 0, &mut files);
        }
        let applications = files
            .into_iter()
            .filter_map(|(id, file)| {
                let text = read_text(&file, MAX_DESKTOP_FILE_BYTES)?;
                let application = desktop_application_from_text(
                    &text,
                    &id,
                    &file.to_string_lossy(),
                    &locale_keys,
                    &desktops,
                )?;
                // Leftovers of uninstalled programs name a binary that is gone.
                if let Some(try_exec) = &application.try_exec
                    && find_program(try_exec, &path_var).is_none()
                {
                    return None;
                }
                let program = split_exec(&application.exec)?.into_iter().next()?;
                find_program(&program, &path_var)?;
                Some(application)
            })
            .collect();

        let mime_dirs: Vec<PathBuf> = data_dirs.iter().map(|d| d.join("mime")).collect();
        let globs = mime_dirs
            .iter()
            .filter_map(|dir| read_text(&dir.join("globs2"), 4 * 1024 * 1024))
            .flat_map(|text| parse_mime_globs(&text))
            .collect();
        let merge_pairs = |name: &str| {
            let mut merged: HashMap<String, Vec<String>> = HashMap::new();
            for dir in &mime_dirs {
                if let Some(text) = read_text(&dir.join(name), 4 * 1024 * 1024) {
                    for (key, values) in parse_mime_pairs(&text) {
                        merged.entry(key).or_insert(values);
                    }
                }
            }
            merged
        };
        let prefixes: Vec<String> = desktops
            .iter()
            .map(|d| format!("{}-", d.to_lowercase()))
            .collect();
        let mut list_files: Vec<PathBuf> = config_directories(env, home)
            .into_iter()
            .chain(data_dirs.iter().map(|d| d.join("applications")))
            .flat_map(|dir| {
                prefixes
                    .iter()
                    .map(|prefix| dir.join(format!("{prefix}mimeapps.list")))
                    .chain(std::iter::once(dir.join("mimeapps.list")))
                    .collect::<Vec<_>>()
            })
            .collect();
        // The legacy defaults.list only ever supplies defaults, after everything else.
        list_files.extend(
            data_dirs
                .iter()
                .map(|d| d.join("applications/defaults.list")),
        );
        let mime_apps = list_files
            .iter()
            .filter_map(|file| read_text(file, MAX_DESKTOP_FILE_BYTES))
            .map(|text| parse_mime_apps_list(&text))
            .collect();
        Self {
            applications,
            globs,
            subclasses: merge_pairs("subclasses"),
            aliases: merge_pairs("aliases"),
            mime_apps,
        }
    }

    /// Every program, ranked for a file called `file_name`.
    pub fn applications_for(&self, file_name: &str) -> Vec<ApplicationChoice> {
        let mime_type = mime_type_for_file_name(file_name, &self.globs);
        rank_applications(
            &self.applications,
            mime_type.as_deref(),
            &self.subclasses,
            &self.aliases,
            &self.mime_apps,
        )
    }

    pub fn find(&self, id: &str) -> Option<&DesktopApplication> {
        self.applications.iter().find(|a| a.id == id)
    }
}

/// Start `application` on `target`; it outlives Muxus.
pub fn launch(application: &DesktopApplication, target: &str) -> Result<(), String> {
    let args = expand_exec(application, target)
        .ok_or_else(|| format!("{} has an unusable Exec line", application.name))?;
    let environment: HashMap<String, String> = std::env::vars().collect();
    let mut command = Command::new(&args[0]);
    command
        .args(&args[1..])
        .env_clear()
        .envs(super::local_open::launch_environment(&environment))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    if let Some(dir) = &application.working_directory {
        command.current_dir(dir);
    }
    command
        .spawn()
        .map(|_| ())
        .map_err(|error| format!("{}: {error}", args[0]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(lines: &[&str]) -> String {
        std::iter::once("[Desktop Entry]")
            .chain(lines.iter().copied())
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn application(id: &str, name: &str, mime_types: &[&str], hidden: bool) -> DesktopApplication {
        DesktopApplication {
            id: id.into(),
            file: format!("/usr/share/applications/{id}"),
            name: name.into(),
            exec: name.to_lowercase(),
            mime_types: mime_types.iter().map(|m| m.to_string()).collect(),
            hidden,
            ..DesktopApplication::default()
        }
    }

    fn strings(values: &[&str]) -> Vec<String> {
        values.iter().map(|v| v.to_string()).collect()
    }

    #[test]
    fn derives_the_locale_keys_a_desktop_entry_may_be_translated_under() {
        assert_eq!(
            desktop_locale_keys(Some("de_DE.UTF-8@euro")),
            strings(&["de_DE@euro", "de_DE", "de@euro", "de"])
        );
        assert_eq!(
            desktop_locale_keys(Some("fr_FR.UTF-8")),
            strings(&["fr_FR", "fr"])
        );
        assert!(desktop_locale_keys(Some("C.UTF-8")).is_empty());
        assert!(desktop_locale_keys(Some("POSIX")).is_empty());
        assert!(desktop_locale_keys(None).is_empty());
    }

    #[test]
    fn reads_a_program_with_its_translated_name_icon_and_file_types() {
        let text = [
            "# comment",
            "[Desktop Entry]",
            "Type=Application",
            "Name=Image Viewer",
            "Name[de]=Bildbetrachter",
            "Exec=eog %U",
            "Icon=org.gnome.eog",
            "MimeType=image/png;image/jpeg;",
            "[Desktop Action new-window]",
            "Name=New Window",
            "Exec=eog --new-window",
        ]
        .join("\n");
        let parsed = desktop_application_from_text(
            &text,
            "org.gnome.eog.desktop",
            "/usr/share/applications/org.gnome.eog.desktop",
            &strings(&["de_DE", "de"]),
            &[],
        )
        .unwrap();
        assert_eq!(
            parsed,
            DesktopApplication {
                id: "org.gnome.eog.desktop".into(),
                file: "/usr/share/applications/org.gnome.eog.desktop".into(),
                name: "Bildbetrachter".into(),
                exec: "eog %U".into(),
                icon: Some("org.gnome.eog".into()),
                mime_types: strings(&["image/png", "image/jpeg"]),
                ..DesktopApplication::default()
            }
        );
    }

    #[test]
    fn skips_entries_that_cannot_open_a_file_here() {
        let base = ["Type=Application", "Name=Tool", "Exec=tool %f"];
        let desktops = strings(&["ubuntu", "GNOME"]);
        let parse = |extra: &[&str]| {
            let lines: Vec<&str> = base.iter().chain(extra.iter()).copied().collect();
            desktop_application_from_text(
                &entry(&lines),
                "tool.desktop",
                "/tool.desktop",
                &[],
                &desktops,
            )
        };
        assert!(parse(&[]).is_some());
        assert!(
            desktop_application_from_text(
                &entry(&["Type=Link", "Name=Docs", "URL=https://example.com"]),
                "t",
                "/t",
                &[],
                &desktops
            )
            .is_none()
        );
        assert!(parse(&["Hidden=true"]).is_none());
        assert!(parse(&["Terminal=true"]).is_none());
        assert!(parse(&["OnlyShowIn=KDE;"]).is_none());
        assert!(parse(&["OnlyShowIn=KDE;GNOME;"]).is_some());
        assert!(parse(&["NotShowIn=GNOME;"]).is_none());
        assert!(
            desktop_application_from_text(
                &entry(&["Type=Application", "Name=Tool"]),
                "t",
                "/t",
                &[],
                &desktops
            )
            .is_none()
        );
    }

    #[test]
    fn keeps_no_display_entries_marked_hidden_for_explicit_associations() {
        let parsed = desktop_application_from_text(
            &entry(&[
                "Type=Application",
                "Name=Chrome",
                "Exec=chrome %U",
                "NoDisplay=true",
            ]),
            "google-chrome.desktop",
            "/google-chrome.desktop",
            &[],
            &[],
        )
        .unwrap();
        assert!(parsed.hidden);
    }

    #[test]
    fn splits_quoted_arguments_and_their_escapes() {
        assert_eq!(
            split_exec(r#"sh -c "echo \"a b\" \$HOME" plain\ arg"#),
            Some(strings(&["sh", "-c", "echo \"a b\" $HOME", "plain arg"]))
        );
        assert_eq!(
            split_exec(r#""/opt/My App/app"  %f"#),
            Some(strings(&["/opt/My App/app", "%f"]))
        );
        assert_eq!(split_exec(r#"broken "quote"#), None);
        assert_eq!(split_exec("   "), None);
    }

    #[test]
    fn expands_file_url_and_metadata_field_codes() {
        let target = "/tmp/x y/plan.drawio";
        let expand = |exec: &str| {
            expand_exec(
                &DesktopApplication {
                    exec: exec.into(),
                    name: "Viewer".into(),
                    icon: Some("viewer".into()),
                    file: "/apps/viewer.desktop".into(),
                    ..DesktopApplication::default()
                },
                target,
            )
        };
        assert_eq!(
            expand("/opt/drawio/drawio %U"),
            Some(strings(&[
                "/opt/drawio/drawio",
                "file:///tmp/x%20y/plan.drawio"
            ]))
        );
        assert_eq!(expand("viewer %F"), Some(strings(&["viewer", target])));
        assert_eq!(
            expand("viewer %i --title=%c %f"),
            Some(strings(&[
                "viewer",
                "--icon",
                "viewer",
                "--title=Viewer",
                target
            ]))
        );
        assert_eq!(
            expand("viewer --desktop-file=%k --file=%f"),
            Some(vec![
                "viewer".into(),
                "--desktop-file=/apps/viewer.desktop".into(),
                format!("--file={target}"),
            ])
        );
        assert_eq!(
            expand("viewer --progress=100%% %d %m %f"),
            Some(strings(&["viewer", "--progress=100%", target]))
        );
        assert_eq!(
            expand(
                "/usr/bin/flatpak run --command=drawio --file-forwarding com.jgraph.drawio @@ %f @@"
            ),
            Some(strings(&[
                "/usr/bin/flatpak",
                "run",
                "--command=drawio",
                "--file-forwarding",
                "com.jgraph.drawio",
                "@@",
                target,
                "@@",
            ]))
        );
    }

    #[test]
    fn appends_the_file_for_programs_that_take_no_file_field_code() {
        assert_eq!(
            expand_exec(
                &DesktopApplication {
                    exec: "env FOO=1 /snap/bin/editor".into(),
                    name: "Editor".into(),
                    file: "/e.desktop".into(),
                    ..DesktopApplication::default()
                },
                "/tmp/a.txt"
            ),
            Some(strings(&["env", "FOO=1", "/snap/bin/editor", "/tmp/a.txt"]))
        );
    }

    #[test]
    fn matches_by_weight_specificity_and_case_rules() {
        let globs = parse_mime_globs(
            &[
                "# generated",
                "80:text/html:*.html",
                "50:application/vnd.jgraph.mxfile:*.drawio",
                "50:text/x-c++src:*.C:cs",
                "50:text/x-csrc:*.c",
                "50:application/x-compressed-tar:*.tar.gz",
                "50:application/gzip:*.gz",
                "50:text/x-readme:README*",
                "50:text/x-makefile:makefile",
                "50:image/x-portable-anymap:*.p[bgp]m",
                "10:text/plain:*.txt",
                "50:application/x-font:__NOGLOBS__",
            ]
            .join("\n"),
        );
        let mime = |name: &str| mime_type_for_file_name(name, &globs);
        assert_eq!(mime("Index.HTML").as_deref(), Some("text/html"));
        assert_eq!(
            mime("plan.drawio").as_deref(),
            Some("application/vnd.jgraph.mxfile")
        );
        assert_eq!(mime("main.c").as_deref(), Some("text/x-csrc"));
        assert_eq!(mime("main.C").as_deref(), Some("text/x-c++src"));
        assert_eq!(
            mime("backup.tar.gz").as_deref(),
            Some("application/x-compressed-tar")
        );
        assert_eq!(mime("log.gz").as_deref(), Some("application/gzip"));
        assert_eq!(mime("README.md").as_deref(), Some("text/x-readme"));
        assert_eq!(mime("Makefile").as_deref(), Some("text/x-makefile"));
        assert_eq!(
            mime("photo.pgm").as_deref(),
            Some("image/x-portable-anymap")
        );
        assert_eq!(mime("photo.pxm"), None);
        assert_eq!(mime("unknown"), None);
    }

    #[test]
    fn walks_subclasses_up_to_the_types_a_text_editor_declares() {
        let subclasses = parse_mime_pairs(
            "application/yaml text/plain\nimage/svg+xml application/xml\napplication/xml text/plain",
        );
        assert_eq!(
            mime_type_ancestors("image/svg+xml", &subclasses),
            strings(&["application/xml", "text/plain"])
        );
        assert_eq!(
            mime_type_ancestors("text/x-python", &subclasses),
            strings(&["text/plain"])
        );
        assert!(mime_type_ancestors("text/plain", &subclasses).is_empty());
    }

    fn programs() -> Vec<DesktopApplication> {
        vec![
            application("eog.desktop", "Image Viewer", &["image/png"], false),
            application("gimp.desktop", "GIMP", &["image/png", "image/jpeg"], false),
            application("editor.desktop", "Text Editor", &["text/plain"], false),
            application("chrome-hidden.desktop", "Google Chrome", &[], true),
            application("okular-png.desktop", "Okular", &["image/png"], true),
            application("calculator.desktop", "Calculator", &[], false),
            application("pinta.desktop", "Pinta", &["image/png"], false),
        ]
    }

    #[test]
    fn puts_the_default_first_then_programs_for_the_type_then_the_rest() {
        let subclasses = parse_mime_pairs("image/svg+xml text/plain");
        let aliases = parse_mime_pairs("image/x-png image/png");
        let lists = [parse_mime_apps_list(
            "[Default Applications]\nimage/png=missing.desktop;gimp.desktop;\n[Removed Associations]\nimage/png=pinta.desktop;",
        )];
        let ranked = rank_applications(
            &programs(),
            Some("image/x-png"),
            &subclasses,
            &aliases,
            &lists,
        );
        let summary: Vec<(String, bool, bool)> = ranked
            .iter()
            .map(|c| (c.application.name.clone(), c.recommended, c.is_default))
            .collect();
        assert_eq!(
            summary,
            vec![
                ("GIMP".into(), true, true),
                ("Image Viewer".into(), true, false),
                ("Calculator".into(), false, false),
                ("Pinta".into(), false, false),
                ("Text Editor".into(), false, false),
            ]
        );
    }

    #[test]
    fn recommends_programs_for_broader_types_after_those_for_the_exact_type() {
        let subclasses = parse_mime_pairs("image/svg+xml text/plain");
        let aliases = parse_mime_pairs("image/x-png image/png");
        let ranked = rank_applications(
            &programs(),
            Some("image/svg+xml"),
            &subclasses,
            &aliases,
            &[parse_mime_apps_list(
                "[Added Associations]\nimage/svg+xml=gimp.desktop;",
            )],
        );
        let recommended: Vec<&str> = ranked
            .iter()
            .filter(|c| c.recommended)
            .map(|c| c.application.name.as_str())
            .collect();
        assert_eq!(recommended, ["GIMP", "Text Editor"]);
    }

    #[test]
    fn offers_a_no_display_program_only_where_an_association_names_it() {
        let subclasses = parse_mime_pairs("image/svg+xml text/plain");
        let aliases = parse_mime_pairs("image/x-png image/png");
        let lists = [parse_mime_apps_list(
            "[Default Applications]\ntext/html=chrome-hidden.desktop",
        )];
        let for_html = rank_applications(
            &programs(),
            Some("text/html"),
            &subclasses,
            &aliases,
            &lists,
        );
        assert_eq!(for_html[0].application.name, "Google Chrome");
        assert!(for_html[0].is_default);
        assert!(!for_html.iter().any(|c| c.application.name == "Okular"));
        let for_png = rank_applications(
            &programs(),
            Some("image/png"),
            &subclasses,
            &aliases,
            &lists,
        );
        assert!(!for_png.iter().any(|c| c.application.hidden));
    }

    #[test]
    fn lists_everything_alphabetically_when_the_type_is_unknown() {
        let ranked = rank_applications(&programs(), None, &HashMap::new(), &HashMap::new(), &[]);
        assert!(ranked.iter().all(|c| !c.recommended && !c.is_default));
        let names: Vec<&str> = ranked.iter().map(|c| c.application.name.as_str()).collect();
        assert_eq!(
            names,
            ["Calculator", "GIMP", "Image Viewer", "Pinta", "Text Editor"]
        );
    }

    #[test]
    fn lists_installed_programs_only_ranked_for_the_file() {
        let scratch = tempfile::tempdir().unwrap();
        let home = scratch.path().join("home");
        let data = scratch.path().join("data");
        let bin = scratch.path().join("bin");
        std::fs::create_dir_all(data.join("applications/sub")).unwrap();
        std::fs::create_dir_all(data.join("mime")).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let tool = bin.join("drawio");
        std::fs::write(&tool, "#!/bin/sh\n").unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::write(
            data.join("applications/sub/drawio.desktop"),
            entry(&[
                "Type=Application",
                "Name=draw.io",
                "Exec=drawio %f",
                "MimeType=application/vnd.jgraph.mxfile;",
            ]),
        )
        .unwrap();
        std::fs::write(
            data.join("applications/gone.desktop"),
            entry(&["Type=Application", "Name=Gone", "Exec=not-installed %f"]),
        )
        .unwrap();
        std::fs::write(
            data.join("mime/globs2"),
            "50:application/vnd.jgraph.mxfile:*.drawio\n",
        )
        .unwrap();
        let env: HashMap<String, String> = [
            ("XDG_DATA_HOME", data.to_str().unwrap()),
            ("XDG_DATA_DIRS", "/nonexistent"),
            ("XDG_CONFIG_HOME", home.join(".config").to_str().unwrap()),
            ("PATH", bin.to_str().unwrap()),
        ]
        .into_iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
        let catalog = LinuxApplicationCatalog::read(&env, &home);
        let choices = catalog.applications_for("plan.drawio");
        assert_eq!(choices.len(), 1);
        assert_eq!(choices[0].application.id, "sub-drawio.desktop");
        assert!(choices[0].recommended);
        assert!(catalog.find("sub-drawio.desktop").is_some());
    }
}
