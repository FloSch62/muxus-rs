//! OpenSSH per-user config engine. OpenSSH files remain the interoperable source for connection
//! details while Muxus owns UI metadata separately, so this module does three jobs:
//!
//!  - parse the config (and its Includes) into a *line-preserving* document, so Host blocks can
//!    be edited in place without disturbing anything else (see [`edit`]);
//!  - list the concrete Host aliases for the session manager, each with the block's own options
//!    plus the fully resolved effective settings;
//!  - resolve any target the way `ssh` would: sequential first-obtained-wins option lookup across
//!    matching Host patterns, accumulating IdentityFile, CertificateFile and *Forward directives.
//!
//! Known deviations from ssh_config(5): Match blocks are skipped (their conditions need runtime
//! state we don't have), and Include inside a Host block is preserved verbatim instead of
//! expanded.

pub mod edit;

use crate::types::ConfigChoice;
use crate::types::ConfigForwardExt;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use crate::local_env::LocalEnv;
use crate::paths::{lexical_absolute, path_string};
use crate::types::{
    ConfigExtra, ConfigForward, ForwardType, HostBlockOptions, RequestTty, ResolvedHostSettings,
    SshHostEntry, StrictHostKeyChecking,
};

const MAX_INCLUDE_DEPTH: usize = 8;

/// One `Keyword value` line inside a block, with original casing preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionLine {
    /// Keyword as written ("HostName").
    pub keyword: String,
    /// Lowercased keyword for comparisons.
    pub key: String,
    /// Raw value text (quotes stripped only in `args`).
    pub value: String,
    pub args: Vec<String>,
    /// Index in its file; `None` for lines that were never read from one.
    pub line: Option<usize>,
}

/// A `Host` block tied to its exact lines in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostBlock {
    pub file: PathBuf,
    /// Index of the first prelude comment line (== host_line when none).
    pub comment_start: usize,
    pub host_line: usize,
    /// Exclusive end of the block's lines in its file.
    pub end: usize,
    pub patterns: Vec<String>,
    pub options: Vec<OptionLine>,
    /// Prelude comment text with leading `# ` stripped, newline-joined.
    pub description: Option<String>,
}

/// Config entries in evaluation order: Host blocks and unconditional top-level runs.
#[derive(Debug, Clone, PartialEq, Eq)]
enum SequenceEntry {
    /// Index into `ConfigDocument::blocks`.
    Block(usize),
    /// Top-level options outside any block; they apply to every host.
    Globals(Vec<OptionLine>),
}

/// The parsed config plus every file's lines.
#[derive(Debug, Clone)]
pub struct ConfigDocument {
    pub root_path: PathBuf,
    /// path → file lines, for every file that was read.
    pub files: HashMap<PathBuf, Vec<String>>,
    /// Files in evaluation order (root first). Includes the root even if absent.
    pub file_order: Vec<PathBuf>,
    pub blocks: Vec<HostBlock>,
    sequence: Vec<SequenceEntry>,
    index: SequenceIndex,
    /// First problem encountered (unreadable include, ...); content may be partial.
    pub error: Option<String>,
    /// The account the document's `~` and %-tokens expand against.
    pub env: LocalEnv,
}

impl ConfigDocument {
    /// A document with no files: resolving against it applies only the fallback layer.
    pub fn empty(env: &LocalEnv) -> Self {
        Self {
            root_path: PathBuf::new(),
            files: HashMap::new(),
            file_order: Vec::new(),
            blocks: Vec::new(),
            sequence: Vec::new(),
            index: SequenceIndex::default(),
            error: None,
            env: env.clone(),
        }
    }
}

/// Reads `root_path` and everything it Includes.
pub fn load_config_document(root_path: &Path, env: &LocalEnv) -> ConfigDocument {
    let root = lexical_absolute(root_path);
    let mut doc = ConfigDocument {
        root_path: root.clone(),
        files: HashMap::new(),
        file_order: Vec::new(),
        blocks: Vec::new(),
        sequence: Vec::new(),
        index: SequenceIndex::default(),
        error: None,
        env: env.clone(),
    };
    let mut state = ParseState {
        visited: HashSet::new(),
        // Per ssh_config(5), relative Include paths resolve against the root config's
        // directory, even from included files.
        include_base: root.parent().map(Path::to_path_buf).unwrap_or_default(),
    };
    parse_file(&mut doc, &root, &mut state, 0);
    if !doc.file_order.contains(&root) {
        doc.file_order.insert(0, root);
    }
    doc.index = SequenceIndex::build(&doc);
    doc
}

struct ParseState {
    visited: HashSet<PathBuf>,
    include_base: PathBuf,
}

fn record_error(doc: &mut ConfigDocument, message: String) {
    if doc.error.is_none() {
        doc.error = Some(message);
    }
}

/// `^([A-Za-z][A-Za-z0-9]*)\s*(?:=|\s)\s*(.*?)\s*$` on an already trimmed line.
fn split_config_line(line: &str) -> Option<(&str, &str)> {
    let bytes = line.as_bytes();
    if !bytes.first()?.is_ascii_alphabetic() {
        return None;
    }
    let keyword_end = bytes
        .iter()
        .position(|b| !b.is_ascii_alphanumeric())
        .unwrap_or(bytes.len());
    let keyword = &line[..keyword_end];
    let rest = &line[keyword_end..];
    // A keyword followed by nothing has no separator, so the line doesn't parse.
    let after_ws = rest.trim_start();
    let value = if let Some(after_eq) = after_ws.strip_prefix('=') {
        after_eq
    } else if after_ws.len() < rest.len() {
        // At least one whitespace character separated keyword and value.
        after_ws
    } else {
        return None;
    };
    Some((keyword, value.trim()))
}

fn parse_file(doc: &mut ConfigDocument, file: &Path, state: &mut ParseState, depth: usize) {
    let resolved = lexical_absolute(file);
    if state.visited.contains(&resolved) || depth > MAX_INCLUDE_DEPTH {
        return;
    }
    state.visited.insert(resolved.clone());

    let text = match std::fs::read(&resolved) {
        Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
        Err(err) => {
            // The root config's absence is normal; broken includes are worth surfacing.
            if depth > 0 {
                record_error(
                    doc,
                    format!(
                        "could not read included file {}: {}",
                        path_string(&resolved),
                        io_message(&err)
                    ),
                );
            }
            return;
        }
    };

    let mut lines: Vec<String> = split_lines(&text);
    // A trailing newline produces one empty final element; drop it so appends don't create
    // gaps (serialization always re-adds the final newline).
    if lines.len() > 1 && lines.last().is_some_and(String::is_empty) {
        lines.pop();
    }
    doc.files.insert(resolved.clone(), lines.clone());
    doc.file_order.push(resolved.clone());

    let mut block: Option<usize> = None;
    let mut in_match = false;
    // Index into the sequence of the open top-level run, if any.
    let mut globals: Option<usize> = None;

    for (i, raw) in lines.iter().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((keyword, value)) = split_config_line(line) else {
            continue;
        };
        let key = keyword.to_ascii_lowercase();
        let args = match split_args(value) {
            Ok(args) => args,
            Err(_) => {
                record_error(
                    doc,
                    format!("{} line {}: invalid quotes", path_string(&resolved), i + 1),
                );
                // Keep malformed option lines inside their block's editable range, but don't let
                // an invalid Host/Match header leak its options into globals.
                if key == "host" || key == "match" {
                    block = None;
                    in_match = true;
                    globals = None;
                } else if let (Some(b), false) = (block, in_match) {
                    doc.blocks[b].end = i + 1;
                }
                continue;
            }
        };
        let option = OptionLine {
            keyword: keyword.to_string(),
            key: key.clone(),
            value: value.to_string(),
            args,
            line: Some(i),
        };

        if key == "host" {
            let comment_start = scan_prelude_comments(&lines, i);
            let patterns: Vec<String> = option
                .args
                .iter()
                .filter(|a| !a.is_empty())
                .cloned()
                .collect();
            doc.blocks.push(HostBlock {
                file: resolved.clone(),
                comment_start,
                host_line: i,
                end: i + 1,
                patterns,
                options: Vec::new(),
                description: prelude_text(&lines, comment_start, i),
            });
            let index = doc.blocks.len() - 1;
            doc.sequence.push(SequenceEntry::Block(index));
            block = Some(index);
            in_match = false;
            globals = None;
            continue;
        }
        if key == "match" {
            block = None;
            in_match = true;
            globals = None;
            continue;
        }
        if key == "include" && block.is_none() && !in_match {
            for pattern in &option.args {
                for included in expand_include_path(pattern, &state.include_base, &doc.env) {
                    parse_file(doc, &included, state, depth + 1);
                }
            }
            globals = None; // included content interleaves; keep evaluation order exact
            continue;
        }
        if in_match {
            continue; // Match-conditioned options are runtime-dependent; skip
        }
        if let Some(b) = block {
            doc.blocks[b].options.push(option);
            doc.blocks[b].end = i + 1;
            continue;
        }
        let run = match globals {
            Some(run) => run,
            None => {
                doc.sequence.push(SequenceEntry::Globals(Vec::new()));
                let run = doc.sequence.len() - 1;
                globals = Some(run);
                run
            }
        };
        if let SequenceEntry::Globals(options) = &mut doc.sequence[run] {
            options.push(option);
        }
    }
}

/// `text.split(/\r?\n/)`.
pub(crate) fn split_lines(text: &str) -> Vec<String> {
    text.split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect()
}

/// Node-style error text for a failed read ("ENOENT: no such file or directory, ...").
fn io_message(err: &std::io::Error) -> String {
    err.to_string()
}

/// Walk back over the contiguous `#` lines directly above a Host line.
fn scan_prelude_comments(lines: &[String], host_line: usize) -> usize {
    let mut j = host_line;
    while j > 0 && lines[j - 1].trim().starts_with('#') {
        j -= 1;
    }
    j
}

fn prelude_text(lines: &[String], comment_start: usize, host_line: usize) -> Option<String> {
    if comment_start >= host_line {
        return None;
    }
    let text = lines[comment_start..host_line]
        .iter()
        .map(|l| {
            let t = l.trim();
            let t = t.strip_prefix('#').unwrap_or(t);
            // `^#\s?`: one whitespace character after the hash goes with it.
            let mut chars = t.chars();
            match chars.next() {
                Some(c) if c.is_whitespace() => chars.as_str(),
                _ => t,
            }
            .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n");
    let text = text.trim();
    (!text.is_empty()).then(|| text.to_string())
}

/// Unterminated quote in a config value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid quotes")]
pub struct InvalidQuotes;

/// Split a config value like OpenSSH's argv_split: single and double quotes may appear anywhere
/// in a token, and basic escapes remove their leading backslash. Other backslashes (notably
/// those in Windows paths) remain untouched.
pub fn split_args(value: &str) -> Result<Vec<String>, InvalidQuotes> {
    let chars: Vec<char> = value.chars().collect();
    let mut out = Vec::new();
    let mut token = String::new();
    let mut quote: Option<char> = None;
    let mut started = false;
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        let next = chars.get(i + 1).copied();
        if c == '\\' && matches!(next, Some('\'') | Some('"') | Some('\\'))
            || (c == '\\' && quote.is_none() && next == Some(' '))
        {
            token.push(next.unwrap_or_default());
            i += 2;
            started = true;
            continue;
        } else if quote.is_none() && (c == '\'' || c == '"') {
            quote = Some(c);
            started = true;
        } else if quote == Some(c) {
            quote = None;
        } else if (c == ' ' || c == '\t') && quote.is_none() {
            if started {
                out.push(std::mem::take(&mut token));
                started = false;
            }
        } else {
            token.push(c);
            started = true;
        }
        i += 1;
    }
    if quote.is_some() {
        return Err(InvalidQuotes);
    }
    if started {
        out.push(token);
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Pattern matching (ssh_config PATTERNS)
// ---------------------------------------------------------------------------

/// `*` matches any run, `?` any one character; everything else is literal.
pub(crate) fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || (p[pi] != '*' && p[pi] == t[ti])) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn has_wildcard(text: &str) -> bool {
    text.contains(['*', '?'])
}

/// Match a Host line's pattern list: any positive match, unless a negation matches.
pub fn host_patterns_match<S: AsRef<str>>(patterns: &[S], host: &str) -> bool {
    let mut matched = false;
    for p in patterns {
        let p = p.as_ref();
        if p.is_empty() {
            continue;
        }
        if let Some(negated) = p.strip_prefix('!') {
            if glob_match(negated, host) {
                return false;
            }
        } else if glob_match(p, host) {
            matched = true;
        }
    }
    matched
}

/// A concrete, connectable alias: no wildcards, not a negation.
pub fn is_concrete_alias(pattern: &str) -> bool {
    !pattern.is_empty() && !pattern.starts_with('!') && !has_wildcard(pattern)
}

/// Which sequence entries can apply to which host. Resolving walks the config in order, and
/// walking every entry for every alias is quadratic: a config with a few thousand Host blocks
/// spent seconds listing them. But a block whose patterns are all concrete names can only ever
/// match those names (a wildcard-free glob is plain equality), so only wildcard blocks and the
/// top-level runs have to be tested against every host.
#[derive(Debug, Clone, Default)]
struct SequenceIndex {
    /// Positions tested for every host: unconditional runs and wildcard blocks.
    general: Vec<usize>,
    /// Positions of blocks whose positive patterns are all concrete, by name.
    by_name: HashMap<String, Vec<usize>>,
}

impl SequenceIndex {
    fn build(doc: &ConfigDocument) -> Self {
        let mut index = SequenceIndex::default();
        for (position, entry) in doc.sequence.iter().enumerate() {
            let SequenceEntry::Block(b) = entry else {
                index.general.push(position);
                continue;
            };
            // A block with only negations never matches; host_patterns_match agrees.
            let positive: Vec<&String> = doc.blocks[*b]
                .patterns
                .iter()
                .filter(|p| !p.is_empty() && !p.starts_with('!'))
                .collect();
            if positive.iter().any(|p| has_wildcard(p)) {
                index.general.push(position);
                continue;
            }
            let mut seen = HashSet::new();
            for name in positive {
                if seen.insert(name) {
                    index
                        .by_name
                        .entry(name.clone())
                        .or_default()
                        .push(position);
                }
            }
        }
        index
    }
}

impl ConfigDocument {
    /// The entries that can match `host`, in config order. Negations are still for the caller to
    /// test.
    fn candidate_entries(&self, host: &str) -> Vec<&SequenceEntry> {
        let general = &self.index.general;
        let named: &[usize] = self
            .index
            .by_name
            .get(host)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        let mut entries = Vec::with_capacity(general.len() + named.len());
        let (mut g, mut n) = (0, 0);
        while g < general.len() || n < named.len() {
            let next = if n >= named.len() || (g < general.len() && general[g] < named[n]) {
                g += 1;
                general[g - 1]
            } else {
                n += 1;
                named[n - 1]
            };
            entries.push(&self.sequence[next]);
        }
        entries
    }

    fn entry_parts<'a>(
        &'a self,
        entry: &'a SequenceEntry,
    ) -> (Option<&'a [String]>, &'a [OptionLine]) {
        match entry {
            SequenceEntry::Block(b) => {
                let block = &self.blocks[*b];
                (Some(&block.patterns), &block.options)
            }
            SequenceEntry::Globals(options) => (None, options),
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution (what `ssh <host>` would use)
// ---------------------------------------------------------------------------

/// Resolved settings plus connection tunables the shared DTO doesn't carry.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedTarget {
    pub hostname: String,
    pub user: Option<String>,
    pub port: u16,
    pub identity_files: Vec<String>,
    pub certificate_files: Vec<String>,
    pub identities_only: bool,
    /// Agent socket override: a path, `$VAR`/`${VAR}`, `SSH_AUTH_SOCK`, or `none`.
    pub identity_agent: Option<String>,
    pub forward_agent: bool,
    pub forward_x11: Option<bool>,
    pub proxy_jump: Vec<String>,
    pub proxy_command: Option<String>,
    pub forwards: Vec<ConfigForward>,
    pub password_only: bool,
    pub connect_timeout: Option<f64>,
    pub server_alive_interval: Option<f64>,
    pub server_alive_count_max: Option<f64>,
    /// Raw ssh_config algorithm lists; translated to the client's form at dial time.
    pub ciphers: Option<String>,
    pub kex_algorithms: Option<String>,
    pub host_key_algorithms: Option<String>,
    pub macs: Option<String>,
    pub compression: Option<bool>,
    /// FIDO middleware for security key files: a library path, `$VAR`, or `internal`.
    pub security_key_provider: Option<String>,
    /// `Some(false)` ⇒ never try this method (`PasswordAuthentication no`).
    pub password_authentication: Option<bool>,
    pub kbd_interactive_authentication: Option<bool>,
    /// Expanded paths; `Some([])` means `none`. `None` ⇒ OpenSSH defaults.
    pub user_known_hosts_files: Option<Vec<String>>,
    pub global_known_hosts_files: Option<Vec<String>>,
    /// First-obtained value per variable, ssh_config SetEnv order.
    pub set_env: BTreeMap<String, String>,
    /// SendEnv patterns in obtained order, `-` removals included.
    pub send_env: Vec<String>,
    pub remote_command: Option<String>,
    pub request_tty: Option<RequestTty>,
    pub strict_host_key_checking: Option<StrictHostKeyChecking>,
}

impl ResolvedTarget {
    /// The shared DTO subset.
    pub fn settings(&self) -> ResolvedHostSettings {
        ResolvedHostSettings {
            hostname: self.hostname.clone(),
            user: self.user.clone(),
            port: self.port,
            identity_files: self.identity_files.clone(),
            certificate_files: self.certificate_files.clone(),
            identities_only: self.identities_only,
            identity_agent: self.identity_agent.clone(),
            forward_agent: self.forward_agent,
            forward_x11: self.forward_x11,
            proxy_jump: self.proxy_jump.clone(),
            proxy_command: self.proxy_command.clone(),
            forwards: self.forwards.clone(),
            password_only: self.password_only,
        }
    }
}

fn yes(v: Option<&String>) -> bool {
    v.is_some_and(|v| v.eq_ignore_ascii_case("yes"))
}

const RESOLVED_KEYS: &[&str] = &[
    "hostname",
    "user",
    "port",
    "identitiesonly",
    "forwardagent",
    "forwardx11",
    "preferredauthentications",
    "pubkeyauthentication",
    "connecttimeout",
    "serveraliveinterval",
    "serveralivecountmax",
    "ciphers",
    "kexalgorithms",
    "hostkeyalgorithms",
    "macs",
    "compression",
    "identityagent",
    "passwordauthentication",
    "kbdinteractiveauthentication",
    "userknownhostsfile",
    "globalknownhostsfile",
    "remotecommand",
    "requesttty",
    "stricthostkeychecking",
    "securitykeyprovider",
];

/// Values whose consumers need the complete argument list or command text.
const RAW_RESOLVED_VALUE_KEYS: &[&str] = &[
    "globalknownhostsfile",
    "remotecommand",
    "userknownhostsfile",
];

/// One entry to evaluate: its Host patterns (`None` applies to every host), its options, and
/// whether it is the folder fallback layer.
type ResolveStep<'a> = (Option<&'a [String]>, &'a [OptionLine], bool);

/// Resolve every option for `host` in ssh's sequential first-obtained-wins order. IdentityFile,
/// CertificateFile and the *Forward directives accumulate instead. ProxyJump and ProxyCommand
/// are mutually exclusive: whichever is obtained first wins, matching OpenSSH.
///
/// `fallback` options (Muxus folder defaults) are evaluated after the entire config, as if
/// appended at the end of ~/.ssh/config in a block matching only this host: first-obtained-wins
/// makes them fill gaps without ever beating an option the user wrote in ssh_config.
pub fn resolve_host(
    doc: &ConfigDocument,
    host: &str,
    fallback: Option<&[OptionLine]>,
) -> ResolvedTarget {
    let mut first: HashMap<&'static str, String> = HashMap::new();
    let mut identity_files: Vec<String> = Vec::new();
    let mut certificate_files: Vec<String> = Vec::new();
    let mut forwards: Vec<ConfigForward> = Vec::new();
    let mut set_env: BTreeMap<String, String> = BTreeMap::new();
    let mut send_env: Vec<String> = Vec::new();

    let mut sequence: Vec<ResolveStep<'_>> = doc
        .candidate_entries(host)
        .into_iter()
        .map(|entry| {
            let (patterns, options) = doc.entry_parts(entry);
            (patterns, options, false)
        })
        .collect();
    if let Some(fallback) = fallback.filter(|f| !f.is_empty()) {
        sequence.push((None, fallback, true));
    }
    for (patterns, options, is_fallback) in sequence {
        if let Some(patterns) = patterns
            && !host_patterns_match(patterns, host)
        {
            continue;
        }
        // IdentityFile is normally cumulative, but folder options are a fallback layer rather
        // than another ssh_config block. Once the config supplied a key, omit all folder keys
        // instead of offering them after it.
        let config_provided_identity_file = is_fallback && !identity_files.is_empty();
        for opt in options {
            // OpenSSH treats ChallengeResponseAuthentication as an exact alias, so both
            // spellings share one first-obtained slot.
            let resolved_key = if opt.key == "challengeresponseauthentication" {
                "kbdinteractiveauthentication"
            } else {
                opt.key.as_str()
            };
            match opt.key.as_str() {
                "identityfile" => {
                    if config_provided_identity_file {
                        continue;
                    }
                    if let Some(file) = opt.args.first().filter(|f| !f.is_empty())
                        && !identity_files.contains(file)
                    {
                        identity_files.push(file.clone());
                    }
                }
                "setenv" => {
                    for arg in &opt.args {
                        let Some(eq) = arg.find('=') else { continue };
                        if eq == 0 {
                            continue;
                        }
                        let name = &arg[..eq];
                        set_env
                            .entry(name.to_string())
                            .or_insert_with(|| arg[eq + 1..].to_string());
                    }
                }
                "sendenv" => send_env.extend(opt.args.iter().filter(|a| !a.is_empty()).cloned()),
                "certificatefile" => {
                    if let Some(file) = opt.args.first().filter(|f| !f.is_empty())
                        && !certificate_files.contains(file)
                    {
                        certificate_files.push(file.clone());
                    }
                }
                "proxyjump" | "proxycommand" => {
                    let is_jump = opt.key == "proxyjump";
                    let value = if is_jump {
                        opt.args.first().cloned()
                    } else {
                        Some(opt.value.clone())
                    };
                    if !first.contains_key("proxyjump")
                        && !first.contains_key("proxycommand")
                        && let Some(value) = value.filter(|v| !v.is_empty())
                    {
                        first.insert(if is_jump { "proxyjump" } else { "proxycommand" }, value);
                    }
                }
                "localforward" | "remoteforward" | "dynamicforward" => {
                    if let Some(fwd) = parse_forward_option(&opt.key, &opt.args) {
                        forwards.push(fwd);
                    }
                }
                _ => {
                    let Some(key) = RESOLVED_KEYS.iter().find(|k| **k == resolved_key) else {
                        continue;
                    };
                    let value = if RAW_RESOLVED_VALUE_KEYS.contains(key) {
                        Some(opt.value.clone())
                    } else {
                        opt.args.first().cloned()
                    };
                    if !first.contains_key(key)
                        && let Some(value) = value.filter(|v| !v.is_empty())
                    {
                        first.insert(key, value);
                    }
                }
            }
        }
    }

    let env = &doc.env;
    let hostname = expand_tokens(
        first.get("hostname").map(String::as_str).unwrap_or(host),
        host,
    );
    let user = first.get("user").cloned();
    let port = parse_port(first.get("port").map(String::as_str)).unwrap_or(22);
    let preferred = first.get("preferredauthentications");
    let remote_user = user.clone().unwrap_or_else(|| env.username.clone());
    let proxy_jump = first.get("proxyjump");
    let known_hosts_tokens = KnownHostsPathTokens {
        h: hostname.clone(),
        n: host.to_string(),
        p: port,
        r: remote_user.clone(),
        j: match proxy_jump {
            Some(j) if j.eq_ignore_ascii_case("none") => String::new(),
            Some(j) => j.clone(),
            None => String::new(),
        },
        k: host.to_string(),
    };
    let identity_tokens = IdentityTokens {
        h: &hostname,
        r: &remote_user,
    };

    ResolvedTarget {
        identity_files: identity_files
            .iter()
            .map(|f| expand_identity_path(f, &identity_tokens, env))
            .collect(),
        certificate_files: certificate_files
            .iter()
            .map(|f| expand_identity_path(f, &identity_tokens, env))
            .collect(),
        identities_only: yes(first.get("identitiesonly")),
        forward_agent: yes(first.get("forwardagent")),
        forward_x11: flag(first.get("forwardx11")),
        proxy_jump: parse_proxy_jump_list(first.get("proxyjump").map(String::as_str)),
        proxy_command: parse_proxy_command(first.get("proxycommand").map(String::as_str)),
        forwards,
        password_only: first
            .get("pubkeyauthentication")
            .is_some_and(|v| v.eq_ignore_ascii_case("no"))
            || preferred
                .is_some_and(|p| !p.to_ascii_lowercase().split(',').any(|m| m == "publickey")),
        connect_timeout: parse_number(first.get("connecttimeout")),
        server_alive_interval: parse_number(first.get("serveraliveinterval")),
        server_alive_count_max: parse_number(first.get("serveralivecountmax")),
        ciphers: first.get("ciphers").cloned(),
        kex_algorithms: first.get("kexalgorithms").cloned(),
        host_key_algorithms: first.get("hostkeyalgorithms").cloned(),
        macs: first.get("macs").cloned(),
        compression: flag(first.get("compression")),
        identity_agent: parse_identity_agent(first.get("identityagent"), &identity_tokens, env),
        security_key_provider: first.get("securitykeyprovider").cloned(),
        password_authentication: flag(first.get("passwordauthentication")),
        kbd_interactive_authentication: flag(first.get("kbdinteractiveauthentication")),
        user_known_hosts_files: parse_known_hosts_files(
            first.get("userknownhostsfile"),
            &known_hosts_tokens,
            env,
        ),
        global_known_hosts_files: parse_known_hosts_files(
            first.get("globalknownhostsfile"),
            &known_hosts_tokens,
            env,
        ),
        set_env,
        send_env,
        remote_command: parse_remote_command(first.get("remotecommand").map(String::as_str)),
        request_tty: first.get("requesttty").and_then(|v| RequestTty::parse(v)),
        strict_host_key_checking: first
            .get("stricthostkeychecking")
            .and_then(|v| StrictHostKeyChecking::parse(v)),
        hostname,
        user,
        port,
    }
}

/// yes/no → bool; anything else (including unset) → `None`.
fn flag(value: Option<&String>) -> Option<bool> {
    match value.map(|v| v.to_ascii_lowercase()).as_deref() {
        Some("yes") => Some(true),
        Some("no") => Some(false),
        _ => None,
    }
}

/// `none`, `$VAR`/`${VAR}`, and `SSH_AUTH_SOCK` indirections pass through; paths expand.
fn parse_identity_agent(
    value: Option<&String>,
    tokens: &IdentityTokens<'_>,
    env: &LocalEnv,
) -> Option<String> {
    let value = value.filter(|v| !v.is_empty())?;
    if value.eq_ignore_ascii_case("none") || value.starts_with('$') || value == "SSH_AUTH_SOCK" {
        return Some(value.clone());
    }
    Some(expand_identity_path(value, tokens, env))
}

/// Space-separated path list; `none` → `[]`; unset → `None` (defaults).
fn parse_known_hosts_files(
    value: Option<&String>,
    tokens: &KnownHostsPathTokens,
    env: &LocalEnv,
) -> Option<Vec<String>> {
    let value = value?;
    let args: Vec<String> = split_args(value)
        .unwrap_or_default()
        .into_iter()
        .filter(|a| !a.is_empty())
        .collect();
    if args.len() == 1 && args[0].eq_ignore_ascii_case("none") {
        return Some(Vec::new());
    }
    Some(
        args.iter()
            .map(|f| expand_known_hosts_path(f, tokens, env))
            .collect(),
    )
}

/// The environment for a session channel: process variables matched by the SendEnv patterns
/// (later `-pattern` entries remove earlier matches, as in ssh_config), overridden by explicit
/// SetEnv values. `None` when empty.
pub fn session_environment<'a, I>(
    set_env: &BTreeMap<String, String>,
    send_env: &[String],
    source: I,
) -> Option<BTreeMap<String, String>>
where
    I: IntoIterator<Item = (&'a str, &'a str)>,
{
    let source: Vec<(&str, &str)> = source.into_iter().collect();
    let mut names: Vec<&str> = Vec::new();
    for pattern in send_env {
        if let Some(removal) = pattern.strip_prefix('-') {
            names.retain(|name| !glob_match(removal, name));
        } else {
            for (name, _) in &source {
                if glob_match(pattern, name) && !names.contains(name) {
                    names.push(name);
                }
            }
        }
    }
    let mut env: BTreeMap<String, String> = BTreeMap::new();
    for name in names {
        if let Some((_, value)) = source.iter().find(|(n, _)| *n == name) {
            env.insert(name.to_string(), value.to_string());
        }
    }
    env.extend(set_env.iter().map(|(k, v)| (k.clone(), v.clone())));
    (!env.is_empty()).then_some(env)
}

/// [`session_environment`] against this process's environment.
pub fn process_session_environment(target: &ResolvedTarget) -> Option<BTreeMap<String, String>> {
    let vars: Vec<(String, String)> = std::env::vars().collect();
    session_environment(
        &target.set_env,
        &target.send_env,
        vars.iter().map(|(k, v)| (k.as_str(), v.as_str())),
    )
}

fn parse_proxy_command(value: Option<&str>) -> Option<String> {
    value
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("none"))
        .map(str::to_string)
}

fn parse_remote_command(value: Option<&str>) -> Option<String> {
    value
        .filter(|v| !v.is_empty() && !v.eq_ignore_ascii_case("none"))
        .map(str::to_string)
}

/// A JavaScript `Number(value)` for config values: decimal (with optional fraction/exponent),
/// surrounding whitespace allowed, empty string is 0.
pub(crate) fn js_number(value: &str) -> Option<f64> {
    let v = value.trim();
    if v.is_empty() {
        return Some(0.0);
    }
    if let Some(hex) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok().map(|n| n as f64);
    }
    if v.chars()
        .any(|c| c.is_ascii_alphabetic() && c != 'e' && c != 'E')
    {
        return match v {
            "Infinity" | "+Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => None,
        };
    }
    v.parse::<f64>().ok()
}

pub(crate) fn parse_port(value: Option<&str>) -> Option<u16> {
    let n = js_number(value?)?;
    (n.fract() == 0.0 && n > 0.0 && n < 65536.0).then_some(n as u16)
}

fn parse_number(value: Option<&String>) -> Option<f64> {
    let n = js_number(value?)?;
    (n.is_finite() && n >= 0.0).then_some(n)
}

/// `ProxyJump a,user@b:2222,c` → hop specs in dialing order; `none` → `[]`.
pub fn parse_proxy_jump_list(value: Option<&str>) -> Vec<String> {
    match value {
        None => Vec::new(),
        Some(v) if v.is_empty() || v.eq_ignore_ascii_case("none") => Vec::new(),
        Some(v) => v
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect(),
    }
}

/// `[user@]host[:port]`: a ProxyJump hop or quick-connect target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostSpec {
    pub host: String,
    pub user: Option<String>,
    pub port: Option<u16>,
}

pub fn parse_host_spec(spec: &str) -> HostSpec {
    let mut rest = spec.trim();
    let mut user = None;
    if let Some(at) = rest.rfind('@')
        && at > 0
    {
        user = Some(rest[..at].to_string()).filter(|u| !u.is_empty());
        rest = &rest[at + 1..];
    }
    // [v6::addr]:port / bare v6 addresses keep their colons.
    if let Some(inner) = rest.strip_prefix('[')
        && let Some(close) = inner.find(']')
    {
        let host = &inner[..close];
        let tail = &inner[close + 1..];
        if !host.is_empty() && !host.contains(']') {
            if tail.is_empty() {
                return HostSpec {
                    host: host.to_string(),
                    user,
                    port: None,
                };
            }
            if let Some(port) = tail.strip_prefix(':')
                && !port.is_empty()
                && port.bytes().all(|b| b.is_ascii_digit())
            {
                return HostSpec {
                    host: host.to_string(),
                    user,
                    port: parse_port(Some(port)),
                };
            }
        }
    }
    if let Some(colon) = rest.rfind(':')
        && colon > 0
        && !rest[..colon].contains(':')
        && let Some(port) = parse_port(Some(&rest[colon + 1..]))
    {
        return HostSpec {
            host: rest[..colon].to_string(),
            user,
            port: Some(port),
        };
    }
    HostSpec {
        host: rest.to_string(),
        user,
        port: None,
    }
}

/// Expand the %-tokens ssh allows in HostName (%h = originally requested name).
fn expand_tokens(value: &str, host: &str) -> String {
    replace_percent_tokens(value, |token| match token {
        '%' => Some("%".to_string()),
        'h' => Some(host.to_string()),
        _ => None,
    })
}

/// Replaces `%x` tokens the callback knows; unknown ones stay verbatim.
fn replace_percent_tokens(value: &str, mut lookup: impl FnMut(char) -> Option<String>) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '%'
            && let Some(&next) = chars.peek()
            && let Some(replacement) = lookup(next)
        {
            out.push_str(&replacement);
            chars.next();
            continue;
        }
        out.push(c);
    }
    out
}

/// `~` at the start, followed by the end or a separator, becomes the home directory.
pub(crate) fn expand_tilde(value: &str, env: &LocalEnv) -> String {
    match value.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\') => {
            format!("{}{rest}", path_string(&env.home))
        }
        _ => value.to_string(),
    }
}

/// The tokens identity and certificate paths may use.
pub struct IdentityTokens<'a> {
    pub h: &'a str,
    pub r: &'a str,
}

/// Expand ~ and the common %-tokens in an identity or certificate path.
pub fn expand_identity_path(value: &str, tokens: &IdentityTokens<'_>, env: &LocalEnv) -> String {
    let home = path_string(&env.home);
    replace_percent_tokens(&expand_tilde(value, env), |token| match token {
        '%' => Some("%".to_string()),
        'd' => Some(home.clone()),
        'h' => Some(tokens.h.to_string()),
        'r' => Some(tokens.r.to_string()),
        'u' => Some(env.username.clone()),
        _ => None,
    })
}

struct KnownHostsPathTokens {
    /// Resolved remote hostname.
    h: String,
    /// Original host argument.
    n: String,
    /// Resolved remote port.
    p: u16,
    /// Resolved remote username.
    r: String,
    /// ProxyJump contents, or empty when unset.
    j: String,
    /// HostKeyAlias, or the original host when none is configured.
    k: String,
}

/// Expand the full token set OpenSSH accepts in UserKnownHostsFile paths.
fn expand_known_hosts_path(value: &str, tokens: &KnownHostsPathTokens, env: &LocalEnv) -> String {
    let local_hostname = &env.hostname;
    let connection_hash = hex::encode(Sha1::digest(
        format!(
            "{local_hostname}{}{}{}{}",
            tokens.h, tokens.p, tokens.r, tokens.j
        )
        .as_bytes(),
    ));
    let home = path_string(&env.home);
    replace_percent_tokens(&expand_tilde(value, env), |token| {
        Some(match token {
            '%' => "%".to_string(),
            'C' => connection_hash.clone(),
            'd' => home.clone(),
            'h' => tokens.h.clone(),
            'i' => env.uid.to_string(),
            'j' => tokens.j.clone(),
            'k' => tokens.k.clone(),
            'L' => local_hostname
                .split('.')
                .next()
                .unwrap_or(local_hostname)
                .to_string(),
            'l' => local_hostname.clone(),
            'n' => tokens.n.clone(),
            'p' => tokens.p.to_string(),
            'r' => tokens.r.clone(),
            'u' => env.username.clone(),
            _ => return None,
        })
    })
}

/// Parse a LocalForward/RemoteForward/DynamicForward option's arguments.
pub fn parse_forward_option(key: &str, args: &[String]) -> Option<ConfigForward> {
    let bind_port = parse_listen_port(args.first().map(String::as_str))?;
    let kind = match key {
        "dynamicforward" => return Some(ConfigForward::dynamic(bind_port)),
        "localforward" => ForwardType::Local,
        "remoteforward" => ForwardType::Remote,
        _ => return None,
    };
    let (host, port) = parse_target_spec(args.get(1).map(String::as_str))?;
    Some(ConfigForward::to(kind, bind_port, &host, port))
}

/// `[bind_address:]port` (we always bind loopback; only the port matters).
fn parse_listen_port(spec: Option<&str>) -> Option<u16> {
    let spec = spec.filter(|s| !s.is_empty())?;
    match spec.rfind(':') {
        Some(idx) => parse_port(Some(&spec[idx + 1..])),
        None => parse_port(Some(spec)),
    }
}

/// `host:port` or `host/port`.
fn parse_target_spec(spec: Option<&str>) -> Option<(String, u16)> {
    let spec = spec.filter(|s| !s.is_empty())?;
    let sep = if spec.contains('/') {
        spec.rfind('/')
    } else {
        spec.rfind(':')
    }?;
    if sep == 0 {
        return None;
    }
    let port = parse_port(Some(&spec[sep + 1..]))?;
    let host = &spec[..sep];
    let host = host.strip_prefix('[').unwrap_or(host);
    let host = host.strip_suffix(']').unwrap_or(host);
    (!host.is_empty()).then(|| (host.to_string(), port))
}

// ---------------------------------------------------------------------------
// Listing (the session manager's data)
// ---------------------------------------------------------------------------

/// Map a block's own option lines to the editable DTO; unmodeled lines land in extras.
pub fn block_to_options(block: &HostBlock) -> HostBlockOptions {
    let mut out = HostBlockOptions::default();
    let mut extras: Vec<ConfigExtra> = Vec::new();
    let mut preferred_consumed = false;
    let mut proxy_consumed = false;
    let first_arg = |opt: &OptionLine| opt.args.first().cloned();

    for opt in &block.options {
        let extra = || crate::types::extra(opt.keyword.clone(), opt.value.clone());
        match opt.key.as_str() {
            "hostname" => {
                if out.hostname.is_none() {
                    out.hostname = first_arg(opt);
                } else {
                    extras.push(extra());
                }
            }
            "user" => {
                if out.user.is_none() {
                    out.user = first_arg(opt);
                } else {
                    extras.push(extra());
                }
            }
            "port" => match parse_port(opt.args.first().map(String::as_str)) {
                Some(port) if out.port.is_none() => out.port = Some(port),
                _ => extras.push(extra()),
            },
            "identityfile" => {
                if let Some(file) = opt.args.first().filter(|f| !f.is_empty()) {
                    out.identity_files
                        .get_or_insert_with(Vec::new)
                        .push(file.clone());
                }
            }
            "certificatefile" => {
                if let Some(file) = opt.args.first().filter(|f| !f.is_empty()) {
                    out.certificate_files
                        .get_or_insert_with(Vec::new)
                        .push(file.clone());
                }
            }
            "identitiesonly" => out.identities_only = Some(yes(opt.args.first())),
            "identityagent" => match opt.args.first().filter(|a| !a.is_empty()) {
                Some(agent) if out.identity_agent.is_none() => {
                    out.identity_agent = Some(agent.clone())
                }
                _ => extras.push(extra()),
            },
            "forwardagent" => out.forward_agent = Some(yes(opt.args.first())),
            "forwardx11" => match flag(opt.args.first()) {
                Some(v) if out.forward_x11.is_none() => out.forward_x11 = Some(v),
                _ => extras.push(extra()),
            },
            "proxyjump" => {
                if !proxy_consumed {
                    out.proxy_jump = Some(parse_proxy_jump_list(Some(&opt.value)));
                    proxy_consumed = true;
                } else {
                    extras.push(extra());
                }
            }
            "proxycommand" => {
                if !proxy_consumed {
                    match parse_proxy_command(Some(&opt.value)) {
                        Some(command) => out.proxy_command = Some(command),
                        None => extras.push(extra()),
                    }
                    proxy_consumed = true;
                } else {
                    extras.push(extra());
                }
            }
            "localforward" | "remoteforward" | "dynamicforward" => {
                match parse_forward_option(&opt.key, &opt.args) {
                    Some(fwd) => out.forwards.get_or_insert_with(Vec::new).push(fwd),
                    None => extras.push(extra()),
                }
            }
            "pubkeyauthentication" => {
                if opt
                    .args
                    .first()
                    .is_some_and(|a| a.eq_ignore_ascii_case("no"))
                {
                    out.password_only = Some(true);
                } else {
                    extras.push(extra());
                }
            }
            "preferredauthentications" => {
                // Our canonical passwordOnly pair; anything else is a hand-written policy.
                if opt
                    .value
                    .eq_ignore_ascii_case("keyboard-interactive,password")
                {
                    preferred_consumed = true;
                } else {
                    extras.push(extra());
                }
            }
            "remotecommand" => {
                if out.remote_command.is_none() && !opt.value.is_empty() {
                    out.remote_command = Some(opt.value.clone());
                } else {
                    extras.push(extra());
                }
            }
            "requesttty" => match RequestTty::parse(&opt.value) {
                Some(v) if out.request_tty.is_none() => out.request_tty = Some(v),
                _ => extras.push(extra()),
            },
            "stricthostkeychecking" => match StrictHostKeyChecking::parse(&opt.value) {
                Some(v) if out.strict_host_key_checking.is_none() => {
                    out.strict_host_key_checking = Some(v)
                }
                _ => extras.push(extra()),
            },
            _ => extras.push(extra()),
        }
    }
    if preferred_consumed && out.password_only != Some(true) {
        // PreferredAuthentications without PubkeyAuthentication no: preserve it.
        extras.push(crate::types::extra(
            "PreferredAuthentications",
            "keyboard-interactive,password",
        ));
    }
    if !extras.is_empty() {
        out.extras = Some(extras);
    }
    out
}

/// Folder defaults per alias, for [`list_hosts`].
pub type FallbackFor<'a> = &'a mut dyn FnMut(&str) -> Option<Vec<OptionLine>>;

/// Every connectable alias with its block options and resolved settings. `fallback_for` supplies
/// folder defaults per alias, so resolved values match what connect uses.
pub fn list_hosts(
    doc: &ConfigDocument,
    mut fallback_for: Option<FallbackFor<'_>>,
) -> Vec<SshHostEntry> {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut entries = Vec::new();
    for block in &doc.blocks {
        let concrete: Vec<&String> = block
            .patterns
            .iter()
            .filter(|p| is_concrete_alias(p))
            .collect();
        let Some(alias) = concrete.iter().find(|a| !seen.contains(a.as_str())) else {
            continue;
        };
        let alias = (*alias).clone();
        for a in &concrete {
            seen.insert(a.as_str());
        }
        let fallback = fallback_for.as_mut().and_then(|f| f(&alias));
        let resolved = resolve_host(doc, &alias, fallback.as_deref());
        entries.push(SshHostEntry {
            aliases: concrete.iter().map(|a| (*a).clone()).collect(),
            description: block.description.clone(),
            file: path_string(&block.file),
            options: block_to_options(block),
            resolved: resolved.settings(),
            // Muxus-only metadata is joined in by the session layer.
            metadata: None,
            alias,
        });
    }
    entries
}

/// Resolve an Include argument to concrete files, supporting `~` and filename-level `*`/`?`
/// globs.
fn expand_include_path(pattern: &str, include_base: &Path, env: &LocalEnv) -> Vec<PathBuf> {
    let expanded = expand_tilde(pattern, env);
    let mut p = PathBuf::from(&expanded);
    if !p.is_absolute() {
        p = include_base.join(p);
    }
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !has_wildcard(&name) {
        return vec![p];
    }
    let dir = p.parent().map(Path::to_path_buf).unwrap_or_default();
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| glob_match(&name, f) && !f.contains(['/', '\\']))
        .collect();
    names.sort();
    names.into_iter().map(|f| dir.join(f)).collect()
}

#[cfg(test)]
pub(crate) mod tests;
