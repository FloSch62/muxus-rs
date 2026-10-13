//! Shell integration (port of `client/src/terminal/shell-integration.ts`): command results from
//! OSC 133 (FinalTerm) / OSC 633 (VS Code) become scrollbar marks for failed commands, and the
//! working directory comes from OSC 7 and the `Cwd` property of OSC 133/633.

use std::sync::LazyLock;

use regex::Regex;

/// VS Code's terminalCommandDecoration.errorBackground — the red it uses for failed-command marks
/// in the scrollbar.
pub const ERROR_MARK_COLOR: &str = "#f14c4c";

const MAX_TERMINAL_CWD_LENGTH: usize = 4096;
const CWD_PROPERTY_PREFIX: &str = "P;Cwd=";
const RAW_WINDOWS_CWD_PROPERTY_PREFIX: &str = "P;CwdRaw=";

static WINDOWS_CWD: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:[a-z]:[\\/]|\\\\[^\\/]+[\\/][^\\/]+)").unwrap());
static WINDOWS_DRIVE_URI_PATH: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^/([a-z]:/.*)$").unwrap());

/// Decode the escaping used by OSC 133/633 property values: `\\` and `\xNN`.
fn decode_property_value(value: &str) -> Option<String> {
    let chars: Vec<char> = value.chars().collect();
    let mut decoded = String::new();
    let mut index = 0;
    while index < chars.len() {
        let c = chars[index];
        if c != '\\' {
            decoded.push(c);
            index += 1;
            continue;
        }
        let next = chars.get(index + 1).copied();
        if next == Some('\\') {
            decoded.push('\\');
            index += 2;
            continue;
        }
        let hex: String = chars.iter().skip(index + 2).take(2).collect();
        if next != Some('x') || hex.len() != 2 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
            return None;
        }
        decoded.push(char::from(u8::from_str_radix(&hex, 16).ok()?));
        index += 4;
    }
    Some(decoded)
}

fn windows_terminal_cwd(value: &str) -> bool {
    !value.is_empty() && WINDOWS_CWD.is_match(value)
}

fn valid_terminal_cwd(value: &str) -> bool {
    !value.is_empty()
        && (value.starts_with('/') || windows_terminal_cwd(value))
        && value.encode_utf16().count() <= MAX_TERMINAL_CWD_LENGTH
}

/// Reads current-directory reports from shell integration and standard OSC 7.
#[derive(Debug, Default, Clone)]
pub struct CwdTracker {
    current: Option<String>,
}

impl CwdTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// The last directory reported.
    pub fn current(&self) -> Option<&str> {
        self.current.as_deref()
    }

    /// Handle an OSC 133/633 payload; returns whether it was a directory property. A new
    /// directory is passed to `on_change`.
    pub fn handle_property(&mut self, data: &str, on_change: impl FnOnce(&str)) -> bool {
        if let Some(value) = data.strip_prefix(CWD_PROPERTY_PREFIX) {
            self.report(decode_property_value(value), on_change);
            return true;
        }
        if let Some(value) = data.strip_prefix(RAW_WINDOWS_CWD_PROPERTY_PREFIX) {
            // cmd.exe's $P prompt token cannot apply OSC property escaping. Keep its internal
            // property distinct so standards-compliant Cwd values above always decode doubled
            // backslashes and \xNN escapes.
            self.report(
                windows_terminal_cwd(value).then(|| value.to_string()),
                on_change,
            );
            return true;
        }
        false
    }

    /// Handle an OSC 7 payload (`file://host/path`); returns whether it was a file URI.
    pub fn handle_file_uri(&mut self, data: &str, on_change: impl FnOnce(&str)) -> bool {
        let Ok(uri) = url::Url::parse(data) else {
            return false;
        };
        if uri.scheme() != "file" {
            return false;
        }
        let Some(pathname) = decode_uri_component(uri.path()) else {
            return false;
        };
        let reported = match WINDOWS_DRIVE_URI_PATH.captures(&pathname) {
            Some(drive) => drive[1].replace('/', "\\"),
            None => pathname,
        };
        self.report(Some(reported), on_change);
        true
    }

    fn report(&mut self, cwd: Option<String>, on_change: impl FnOnce(&str)) {
        let Some(cwd) = cwd.filter(|cwd| valid_terminal_cwd(cwd)) else {
            return;
        };
        if self.current.as_deref() == Some(cwd.as_str()) {
            return;
        }
        on_change(&cwd);
        self.current = Some(cwd);
    }
}

/// JavaScript's `decodeURIComponent`: `None` for a malformed escape or invalid UTF-8.
fn decode_uri_component(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    for (index, byte) in bytes.iter().enumerate() {
        if *byte == b'%' {
            let valid = bytes
                .get(index + 1..index + 3)
                .is_some_and(|hex| hex.iter().all(u8::is_ascii_hexdigit));
            if !valid {
                return None;
            }
        }
    }
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()
        .map(|text| text.into_owned())
}

/// The terminal operations the command tracker needs.
pub trait CommandMarkerHost {
    type Marker;
    /// A marker on the cursor's line, or `None` when the buffer cannot hold one.
    fn register_marker(&mut self) -> Option<Self::Marker>;
    /// The marked line has left the buffer.
    fn is_marker_disposed(&self, marker: &Self::Marker) -> bool;
    fn dispose_marker(&mut self, marker: Self::Marker);
    /// Mark the command's line red in the scrollbar's left lane ([`ERROR_MARK_COLOR`]).
    fn mark_failed_command(&mut self, marker: Self::Marker);
}

/// Turns shell-integration command reports into scrollbar marks. OSC 133 / OSC 633 sequences
/// carry "command started" (B / C) and "command finished with exit code" (D;n); a non-zero exit
/// paints the command's line red in the overview ruler, exactly like VS Code's failed-command
/// decorations.
#[derive(Debug)]
pub struct CommandTracker<M> {
    command_start: Option<M>,
}

impl<M> Default for CommandTracker<M> {
    fn default() -> Self {
        Self {
            command_start: None,
        }
    }
}

impl<M> CommandTracker<M> {
    pub fn new() -> Self {
        Self::default()
    }

    /// Handle one OSC 133/633 payload; returns true when consumed.
    pub fn handle<H: CommandMarkerHost<Marker = M>>(&mut self, host: &mut H, data: &str) -> bool {
        let (kind, arg) = match data.find(';') {
            Some(separator) => (&data[..separator], Some(&data[separator + 1..])),
            None => (data, None),
        };
        match kind {
            // Command input starts (prompt end), or the command executes.
            "B" | "C" => {
                if let Some(previous) = self.command_start.take() {
                    host.dispose_marker(previous);
                }
                self.command_start = host.register_marker();
                true
            }
            // Command finished: D;<exit code>
            "D" => {
                let Some(marker) = self.command_start.take() else {
                    return true;
                };
                if host.is_marker_disposed(&marker) {
                    return true;
                }
                match arg.and_then(parse_exit_code) {
                    Some(code) if code != 0 => host.mark_failed_command(marker),
                    _ => host.dispose_marker(marker),
                }
                true
            }
            // Prompt starts: any pending command never reported back.
            "A" => {
                if let Some(previous) = self.command_start.take() {
                    host.dispose_marker(previous);
                }
                true
            }
            _ => false,
        }
    }

    pub fn dispose<H: CommandMarkerHost<Marker = M>>(&mut self, host: &mut H) {
        if let Some(marker) = self.command_start.take() {
            host.dispose_marker(marker);
        }
    }
}

/// `Number.parseInt(arg, 10)`: leading whitespace and sign, then digits; anything after them is
/// ignored.
fn parse_exit_code(arg: &str) -> Option<i64> {
    let trimmed = arg.trim_start();
    let (sign, digits) = match trimmed.as_bytes().first() {
        Some(b'-') => (-1, &trimmed[1..]),
        Some(b'+') => (1, &trimmed[1..]),
        _ => (1, trimmed),
    };
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    if end == 0 {
        return None;
    }
    digits[..end].parse::<i64>().ok().map(|value| sign * value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Default)]
    struct StubTerm {
        markers: Vec<bool>,
        decorations: Vec<usize>,
    }

    impl CommandMarkerHost for StubTerm {
        type Marker = usize;
        fn register_marker(&mut self) -> Option<usize> {
            self.markers.push(false);
            Some(self.markers.len() - 1)
        }
        fn is_marker_disposed(&self, marker: &usize) -> bool {
            self.markers[*marker]
        }
        fn dispose_marker(&mut self, marker: usize) {
            self.markers[marker] = true;
        }
        fn mark_failed_command(&mut self, marker: usize) {
            self.decorations.push(marker);
        }
    }

    fn run(sequences: &[&str]) -> StubTerm {
        let mut term = StubTerm::default();
        let mut tracker = CommandTracker::new();
        for sequence in sequences {
            tracker.handle(&mut term, sequence);
        }
        term
    }

    #[test]
    fn marks_a_failed_command_in_the_overview_ruler() {
        let term = run(&["A", "C", "D;1", "A"]);
        assert_eq!(term.decorations.len(), 1);
        assert!(!term.markers[term.decorations[0]]);
    }

    #[test]
    fn leaves_successful_commands_unmarked_and_frees_their_marker() {
        let term = run(&["A", "C", "D;0", "A"]);
        assert!(term.decorations.is_empty());
        assert!(term.markers[0]);
    }

    #[test]
    fn accepts_b_prompt_end_as_the_command_anchor() {
        assert_eq!(run(&["A", "B", "D;127"]).decorations.len(), 1);
    }

    #[test]
    fn ignores_exit_reports_with_no_preceding_command() {
        // Shells re-run precmd on an empty Enter and re-report the stale $?.
        assert!(run(&["A", "D;1", "A", "D;1"]).decorations.is_empty());
    }

    #[test]
    fn ignores_unparsable_exit_codes() {
        assert!(run(&["C", "D;abc", "C", "D"]).decorations.is_empty());
    }

    #[test]
    fn drops_a_pending_command_when_a_new_prompt_starts_first() {
        let term = run(&["C", "A", "D;1"]);
        assert!(term.decorations.is_empty());
        assert!(term.markers[0]);
    }

    #[test]
    fn marks_each_failure_across_repeated_cycles() {
        assert_eq!(
            run(&["C", "D;1", "A", "C", "D;0", "A", "C", "D;2", "A"])
                .decorations
                .len(),
            2
        );
    }

    #[test]
    fn only_consumes_command_lifecycle_kinds() {
        let mut term = StubTerm::default();
        let mut tracker = CommandTracker::new();
        assert!(tracker.handle(&mut term, "C"));
        assert!(tracker.handle(&mut term, "D;0"));
        assert!(!tracker.handle(&mut term, "P;Cwd=/tmp"));
        assert!(!tracker.handle(&mut term, "E;ls"));
    }

    fn collect(tracker: &mut CwdTracker, paths: &mut Vec<String>, property: &str) -> bool {
        tracker.handle_property(property, |path| paths.push(path.into()))
    }

    #[test]
    fn reports_escaped_current_directory_properties_once_per_path() {
        let (mut tracker, mut paths) = (CwdTracker::new(), Vec::new());
        assert!(collect(
            &mut tracker,
            &mut paths,
            r"P;Cwd=/srv/with\x3bsemi\\backslash"
        ));
        assert!(collect(
            &mut tracker,
            &mut paths,
            r"P;Cwd=/srv/with\x3bsemi\\backslash"
        ));
        assert_eq!(paths, ["/srv/with;semi\\backslash"]);
    }

    #[test]
    fn reports_native_windows_cwd_properties_emitted_by_cmd_prompts() {
        let (mut tracker, mut paths) = (CwdTracker::new(), Vec::new());
        assert!(collect(
            &mut tracker,
            &mut paths,
            r"P;CwdRaw=C:\Users\alice\project"
        ));
        assert!(collect(
            &mut tracker,
            &mut paths,
            r"P;CwdRaw=C:\Users\alice\project"
        ));
        assert!(collect(&mut tracker, &mut paths, r"P;CwdRaw=D:\source"));
        assert_eq!(paths, [r"C:\Users\alice\project", r"D:\source"]);
    }

    #[test]
    fn decodes_standards_escaped_windows_cwd_properties() {
        let (mut tracker, mut paths) = (CwdTracker::new(), Vec::new());
        assert!(collect(
            &mut tracker,
            &mut paths,
            r"P;Cwd=C:\\work\\semi\x3bcolon"
        ));
        assert!(collect(
            &mut tracker,
            &mut paths,
            r"P;Cwd=\\\\server\\share\\dir"
        ));
        assert_eq!(paths, [r"C:\work\semi;colon", r"\\server\share\dir"]);
    }

    #[test]
    fn accepts_standard_file_uris_and_ignores_invalid_or_relative_paths() {
        let (mut tracker, mut paths) = (CwdTracker::new(), Vec::new());
        assert!(
            tracker.handle_file_uri("file://remote.example/srv/my%20app", |p| paths
                .push(p.into()))
        );
        assert!(collect(&mut tracker, &mut paths, "P;Cwd=relative/path"));
        assert!(!tracker.handle_file_uri("https://example.test/srv/app", |p| paths.push(p.into())));
        assert_eq!(paths, ["/srv/my app"]);
    }

    #[test]
    fn converts_windows_osc_7_file_uris_to_native_drive_paths() {
        let (mut tracker, mut paths) = (CwdTracker::new(), Vec::<String>::new());
        assert!(
            tracker.handle_file_uri("file:///C:/Users/alice/my%20project", |p| paths
                .push(p.into()))
        );
        assert_eq!(paths, [r"C:\Users\alice\my project"]);
    }

    #[test]
    fn leaves_unrelated_shell_integration_properties_for_other_handlers() {
        let mut tracker = CwdTracker::new();
        assert!(!tracker.handle_property("C", |_| {}));
        assert!(!tracker.handle_property("P;IsWindows=True", |_| {}));
    }
}
