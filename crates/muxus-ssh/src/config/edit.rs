//! Editing side of the ssh_config engine: serialize a Host block from the editor's DTO and splice
//! it into the user's config files, touching nothing but the edited block's lines. Every write is
//! atomic (tmp + rename), keeps the file's mode, and leaves a `.muxus.bak` copy of the previous
//! content.

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use super::{ConfigDocument, HostBlock, is_concrete_alias, load_config_document};
use crate::error::Problem;
use crate::local_env::LocalEnv;
use crate::paths::{is_within, lexical_absolute, path_string, relative_inside};
use crate::types::ConfigChoice;
use crate::types::{
    ConfigForward, ForwardType, HostOptionsPatch, HostUpsertRequest, Patch, render_patch,
    saved_host_hop_id,
};

/// Keywords that would change the file's structure if smuggled in via extras.
const FORBIDDEN_EXTRAS: &[&str] = &["host", "match"];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpsertResult {
    pub file: PathBuf,
}

/// The block that owns `alias` as a concrete pattern.
pub fn find_host_block(doc: &ConfigDocument, alias: &str) -> Option<usize> {
    doc.blocks.iter().position(|b| {
        b.patterns
            .iter()
            .any(|p| is_concrete_alias(p) && p == alias)
    })
}

fn basename(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// Creates or replaces the Host block `req` describes.
pub fn upsert_host(
    req: &HostUpsertRequest,
    root: &Path,
    env: &LocalEnv,
) -> Result<UpsertResult, Problem> {
    validate_upsert(req)?;
    let mut doc = load_config_document(root, env);

    let mut block: Option<usize> = None;
    if let Some(previous) = &req.previous_alias {
        block = find_host_block(&doc, previous);
        if block.is_none() {
            return Err(Problem::not_found(format!(
                "no Host block for \"{previous}\" in {}",
                basename(&doc.root_path)
            )));
        }
    }
    for alias in &req.aliases {
        if let Some(other) = find_host_block(&doc, alias)
            && Some(other) != block
        {
            return Err(Problem::with_code(
                409,
                format!(
                    "Host \"{alias}\" already exists in {}",
                    basename(&doc.blocks[other].file)
                ),
                "host-exists",
            ));
        }
    }

    let requested = req
        .file
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| block.map(|b| doc.blocks[b].file.clone()))
        .unwrap_or_else(|| doc.root_path.clone());
    let target_file = resolve_target_file(&requested, &doc.root_path)?;
    // Wildcard/negation patterns sharing the edited block's Host line survive edits.
    let extra_patterns = block
        .map(|b| non_concrete(&doc.blocks[b]))
        .unwrap_or_default();
    let rendered = render_host_block(req, &detect_indent(&doc, &target_file), &extra_patterns);

    let mut changed: BTreeSet<PathBuf> = BTreeSet::new();
    match block {
        Some(b) if doc.blocks[b].file == target_file => {
            let HostBlock {
                file,
                comment_start,
                end,
                ..
            } = doc.blocks[b].clone();
            let lines = doc.files.entry(file.clone()).or_default();
            lines.splice(comment_start..end, rendered);
            changed.insert(file);
        }
        _ => {
            if let Some(b) = block {
                let removed = doc.blocks[b].clone();
                remove_block(&mut doc.files, &removed);
                changed.insert(removed.file);
            }
            append_block(&mut doc.files, &target_file, rendered);
            changed.insert(target_file.clone());
            if ensure_included(&mut doc, &target_file) {
                changed.insert(doc.root_path.clone());
            }
        }
    }

    for file in &changed {
        write_config_file(file, doc.files.get(file).map(Vec::as_slice).unwrap_or(&[]))?;
    }
    Ok(UpsertResult { file: target_file })
}

fn non_concrete(block: &HostBlock) -> Vec<String> {
    block
        .patterns
        .iter()
        .filter(|p| !is_concrete_alias(p))
        .cloned()
        .collect()
}

/// One patched keyword, rendered; a `None` value removes the keyword.
struct PatchEntry {
    key: String,
    keyword: &'static str,
    value: Option<String>,
}

/// Apply one option patch to the Host blocks of many aliases: a bulk edit from the sidebar.
/// Unlike an editor save, which re-renders the whole block, only the lines of the patched
/// keywords change: comments, blank lines, spelling and every other option in each block stay
/// byte-identical. Each touched file is written once, so its `.muxus.bak` holds the content from
/// before the whole edit rather than from before its last block. Returns the number of blocks
/// rewritten.
pub fn patch_hosts(
    aliases: &[String],
    patch: &HostOptionsPatch,
    root: &Path,
    env: &LocalEnv,
) -> Result<usize, Problem> {
    let entries = validate_patch(patch)?;
    let mut doc = load_config_document(root, env);
    let mut blocks: Vec<usize> = Vec::new();
    for alias in aliases {
        let Some(block) = find_host_block(&doc, alias) else {
            return Err(Problem::not_found(format!(
                "no Host block for \"{alias}\" in {}",
                basename(&doc.root_path)
            )));
        };
        // The same boundary as an editor save: an Include may pull in files from anywhere, but
        // Muxus only writes under the root config's directory.
        resolve_target_file(&doc.blocks[block].file, &doc.root_path)?;
        if !blocks.contains(&block) {
            blocks.push(block);
        }
    }

    let mut indents: HashMap<PathBuf, String> = HashMap::new();
    for &b in &blocks {
        let file = doc.blocks[b].file.clone();
        indents
            .entry(file.clone())
            .or_insert_with(|| detect_indent(&doc, &file));
    }
    // Bottom-up within each file, so splicing one block never shifts the line range of a block
    // still waiting to be patched.
    let mut ordered: Vec<HostBlock> = blocks.iter().map(|&b| doc.blocks[b].clone()).collect();
    ordered.sort_by(|a, b| a.file.cmp(&b.file).then(b.host_line.cmp(&a.host_line)));
    for block in &ordered {
        let indent = indents
            .get(&block.file)
            .cloned()
            .unwrap_or_else(|| "  ".to_string());
        let lines = doc.files.entry(block.file.clone()).or_default();
        let patched = patch_block_lines(lines, block, &entries, &indent);
        lines.splice(block.host_line..block.end, patched);
    }

    let files: BTreeSet<PathBuf> = ordered.iter().map(|b| b.file.clone()).collect();
    for file in &files {
        write_config_file(file, doc.files.get(file).map(Vec::as_slice).unwrap_or(&[]))?;
    }
    Ok(blocks.len())
}

fn validate_patch(patch: &HostOptionsPatch) -> Result<Vec<PatchEntry>, Problem> {
    if let Patch::Set(user) = &patch.user
        && (user.trim().is_empty() || user.contains(['\r', '\n', '"']))
    {
        return Err(Problem::bad_request(
            "option values must be non-empty single-line text without quotes",
        ));
    }
    if let Patch::Set(port) = patch.port
        && port == 0
    {
        return Err(Problem::bad_request("port must be 1–65535"));
    }
    let yes_no = |value: &bool| {
        if *value {
            "yes".to_string()
        } else {
            "no".to_string()
        }
    };
    let rendered: [(&'static str, Patch<String>); 5] = [
        (
            "User",
            render_patch(&patch.user, |value| quote_token(value.trim())),
        ),
        ("Port", render_patch(&patch.port, |port| port.to_string())),
        ("ForwardAgent", render_patch(&patch.forward_agent, yes_no)),
        ("ForwardX11", render_patch(&patch.forward_x11, yes_no)),
        (
            "StrictHostKeyChecking",
            render_patch(&patch.strict_host_key_checking, |v| v.as_str().to_string()),
        ),
    ];
    let entries: Vec<PatchEntry> = rendered
        .into_iter()
        .filter_map(|(keyword, value)| {
            let value = match value {
                Patch::Keep => return None,
                Patch::Clear => None,
                Patch::Set(v) => Some(v),
            };
            Some(PatchEntry {
                key: keyword.to_ascii_lowercase(),
                keyword,
                value,
            })
        })
        .collect();
    if entries.is_empty() {
        return Err(Problem::bad_request("at least one option is required"));
    }
    Ok(entries)
}

/// The block's lines from its Host line on, with each patched keyword set on its first line
/// (OpenSSH uses the first value it reads) or removed. Later repeats of a patched keyword are
/// dropped so the block ends up saying one thing; a keyword the block lacks is appended after
/// its last line.
fn patch_block_lines(
    lines: &[String],
    block: &HostBlock,
    entries: &[PatchEntry],
    indent: &str,
) -> Vec<String> {
    let mut replace: HashMap<usize, Option<String>> = HashMap::new();
    let mut append: Vec<String> = Vec::new();
    for entry in entries {
        let existing: Vec<_> = block
            .options
            .iter()
            .filter(|o| o.key == entry.key && o.line.is_some())
            .collect();
        for (index, option) in existing.iter().enumerate() {
            let line_no = option.line.unwrap_or_default();
            let keep = index == 0 && entry.value.is_some();
            let current = lines.get(line_no).map(String::as_str).unwrap_or("");
            let lead: String = current.chars().take_while(|c| c.is_whitespace()).collect();
            replace.insert(
                line_no,
                keep.then(|| {
                    format!(
                        "{lead}{} {}",
                        option.keyword,
                        entry.value.as_deref().unwrap_or("")
                    )
                }),
            );
        }
        if existing.is_empty()
            && let Some(value) = &entry.value
        {
            append.push(format!("{indent}{} {value}", entry.keyword));
        }
    }

    let mut out: Vec<String> = Vec::new();
    for index in block.host_line..block.end {
        let next = match replace.get(&index) {
            Some(replacement) => replacement.clone(),
            None => lines.get(index).cloned(),
        };
        if let Some(line) = next {
            out.push(line);
        }
    }
    out.extend(append);
    out
}

/// Removes the Host block that owns `alias`.
pub fn delete_host(alias: &str, root: &Path, env: &LocalEnv) -> Result<(), Problem> {
    let mut doc = load_config_document(root, env);
    let Some(b) = find_host_block(&doc, alias) else {
        return Err(Problem::not_found(format!("no Host block for \"{alias}\"")));
    };
    let block = doc.blocks[b].clone();
    remove_block(&mut doc.files, &block);
    write_config_file(
        &block.file,
        doc.files.get(&block.file).map(Vec::as_slice).unwrap_or(&[]),
    )
}

/// Remove the Host blocks of many aliases, as the sidebar's bulk delete does. Every alias is
/// looked up before anything is written, and each touched file is written once, so its
/// `.muxus.bak` holds the content from before the whole delete rather than from before its last
/// block. Returns the number of blocks removed.
pub fn delete_hosts(aliases: &[String], root: &Path, env: &LocalEnv) -> Result<usize, Problem> {
    let mut doc = load_config_document(root, env);
    let mut blocks: Vec<usize> = Vec::new();
    for alias in aliases {
        let Some(block) = find_host_block(&doc, alias) else {
            return Err(Problem::not_found(format!(
                "no Host block for \"{alias}\" in {}",
                basename(&doc.root_path)
            )));
        };
        if !blocks.contains(&block) {
            blocks.push(block);
        }
    }

    // Bottom-up within each file, so removing one block never shifts the line range of a block
    // still waiting to be removed.
    let mut ordered: Vec<HostBlock> = blocks.iter().map(|&b| doc.blocks[b].clone()).collect();
    ordered.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(b.comment_start.cmp(&a.comment_start))
    });
    for block in &ordered {
        remove_block(&mut doc.files, block);
    }

    let files: BTreeSet<PathBuf> = ordered.iter().map(|b| b.file.clone()).collect();
    for file in &files {
        write_config_file(file, doc.files.get(file).map(Vec::as_slice).unwrap_or(&[]))?;
    }
    Ok(blocks.len())
}

/// The exact text upsert_host would write, for the editor's live preview.
pub fn preview_host(
    req: &HostUpsertRequest,
    root: &Path,
    env: &LocalEnv,
) -> Result<String, Problem> {
    validate_upsert(req)?;
    let doc = load_config_document(root, env);
    let block = req
        .previous_alias
        .as_deref()
        .and_then(|alias| find_host_block(&doc, alias));
    let requested = req
        .file
        .as_ref()
        .map(PathBuf::from)
        .or_else(|| block.map(|b| doc.blocks[b].file.clone()))
        .unwrap_or_else(|| doc.root_path.clone());
    let target_file = resolve_target_file(&requested, &doc.root_path)?;
    let extra_patterns = block
        .map(|b| non_concrete(&doc.blocks[b]))
        .unwrap_or_default();
    Ok(render_host_block(req, &detect_indent(&doc, &target_file), &extra_patterns).join("\n"))
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

fn bad(message: impl Into<String>) -> Problem {
    Problem::bad_request(message)
}

fn valid_alias(alias: &str) -> bool {
    !alias.is_empty()
        && !alias
            .chars()
            .any(|c| c.is_whitespace() || matches!(c, '#' | '*' | '?' | '!'))
}

fn valid_keyword(keyword: &str) -> bool {
    let mut chars = keyword.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric())
}

fn validate_upsert(req: &HostUpsertRequest) -> Result<(), Problem> {
    if req.aliases.is_empty() {
        return Err(bad("at least one alias is required"));
    }
    for alias in &req.aliases {
        if !valid_alias(alias) {
            return Err(bad(format!(
                "invalid alias \"{alias}\" — no spaces, wildcards or \"!\""
            )));
        }
    }
    let o = &req.options;
    let singles = [&o.hostname, &o.user, &o.identity_agent]
        .into_iter()
        .flatten()
        .chain(o.identity_files.iter().flatten())
        .chain(o.certificate_files.iter().flatten());
    for v in singles {
        // These are re-quoted as single tokens on render, so quotes can't nest.
        if v.trim().is_empty() || v.contains(['\r', '\n', '"']) {
            return Err(bad(
                "option values must be non-empty single-line text without quotes",
            ));
        }
    }
    if o.port == Some(0) {
        return Err(bad("port must be 1–65535"));
    }
    for hop in o.proxy_jump.iter().flatten() {
        if hop.trim().is_empty() || hop.chars().any(|c| c.is_whitespace() || c == ',') {
            return Err(bad(format!("invalid jump host \"{hop}\"")));
        }
        // OpenSSH can't resolve a Muxus-owned host, so it never goes into ssh_config.
        if saved_host_hop_id(hop).is_some() {
            return Err(bad(
                "a host stored in Muxus can only be a jump host for other Muxus hosts",
            ));
        }
    }
    if let Some(command) = &o.proxy_command
        && (command.trim().is_empty() || command.contains(['\r', '\n']))
    {
        return Err(bad("ProxyCommand must be non-empty single-line text"));
    }
    if o.proxy_command.is_some() && o.proxy_jump.is_some() {
        return Err(bad("ProxyJump and ProxyCommand are mutually exclusive"));
    }
    if let Some(command) = &o.remote_command
        && (command.trim().is_empty() || command.contains(['\r', '\n']))
    {
        return Err(bad("RemoteCommand must be non-empty single-line text"));
    }
    for f in o.forwards.iter().flatten() {
        validate_forward(f)?;
    }
    for extra in o.extras.iter().flatten() {
        if !valid_keyword(&extra.keyword)
            || FORBIDDEN_EXTRAS.contains(&extra.keyword.to_ascii_lowercase().as_str())
        {
            return Err(bad(format!("invalid option keyword \"{}\"", extra.keyword)));
        }
        // Extras are written verbatim (they may carry their own quoting); only newlines are
        // off-limits.
        if extra.value.contains(['\r', '\n']) {
            return Err(bad(format!("invalid value for {}", extra.keyword)));
        }
    }
    Ok(())
}

fn validate_forward(f: &ConfigForward) -> Result<(), Problem> {
    if f.bind_port == 0 {
        return Err(bad("forward listen port must be 1–65535"));
    }
    if f.kind == ForwardType::Dynamic {
        return Ok(());
    }
    match &f.target_host {
        Some(host) if !host.trim().is_empty() && !host.chars().any(char::is_whitespace) => {}
        _ => return Err(bad("forward target host is required")),
    }
    if !f.target_port.is_some_and(|p| p > 0) {
        return Err(bad("forward target port must be 1–65535"));
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Serialization
// ---------------------------------------------------------------------------

/// Quote one token when OpenSSH would otherwise split or quote it.
pub(crate) fn quote_token(v: &str) -> String {
    // An unquoted apostrophe starts a single-quoted string in OpenSSH. Keep double quotes as our
    // canonical output for both whitespace and apostrophes.
    if v.chars().any(|c| c.is_whitespace() || c == '\'') {
        format!("\"{v}\"")
    } else {
        v.to_string()
    }
}

fn forward_target(host: &str, port: u16) -> String {
    if host.contains(':') {
        format!("[{host}]:{port}")
    } else {
        format!("{host}:{port}")
    }
}

/// The lines of one Host block as the editor would write them.
pub fn render_host_block(
    req: &HostUpsertRequest,
    indent: &str,
    extra_patterns: &[String],
) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let description = req.description.as_deref().unwrap_or("").replace('\r', "");
    let description = description.trim();
    if !description.is_empty() {
        for line in description.split('\n') {
            let line = line.trim();
            lines.push(if line.is_empty() {
                "#".to_string()
            } else {
                format!("# {line}")
            });
        }
    }
    let patterns: Vec<&str> = req
        .aliases
        .iter()
        .chain(extra_patterns.iter())
        .map(String::as_str)
        .collect();
    lines.push(format!("Host {}", patterns.join(" ")));
    let mut opt = |keyword: &str, value: Option<String>| {
        if let Some(value) = value.filter(|v| !v.is_empty()) {
            lines.push(format!("{indent}{keyword} {value}"));
        }
    };
    let single = |v: &Option<String>| v.as_deref().map(|v| quote_token(v.trim()));
    let yes_no = |v: bool| {
        if v {
            "yes".to_string()
        } else {
            "no".to_string()
        }
    };

    let o = &req.options;
    opt("HostName", single(&o.hostname));
    opt("User", single(&o.user));
    opt("Port", o.port.map(|p| p.to_string()));
    for file in o.identity_files.iter().flatten() {
        opt("IdentityFile", Some(quote_token(file.trim())));
    }
    for file in o.certificate_files.iter().flatten() {
        opt("CertificateFile", Some(quote_token(file.trim())));
    }
    opt("IdentitiesOnly", o.identities_only.map(yes_no));
    opt("IdentityAgent", single(&o.identity_agent));
    if let Some(jump) = &o.proxy_jump {
        opt(
            "ProxyJump",
            Some(if jump.is_empty() {
                "none".to_string()
            } else {
                jump.join(",")
            }),
        );
    }
    opt(
        "ProxyCommand",
        o.proxy_command.as_deref().map(|c| c.trim().to_string()),
    );
    opt("ForwardAgent", o.forward_agent.map(yes_no));
    opt("ForwardX11", o.forward_x11.map(yes_no));
    if o.password_only == Some(true) {
        opt("PubkeyAuthentication", Some("no".to_string()));
        opt(
            "PreferredAuthentications",
            Some("keyboard-interactive,password".to_string()),
        );
    }
    for f in o.forwards.iter().flatten() {
        match f.kind {
            ForwardType::Dynamic => opt("DynamicForward", Some(f.bind_port.to_string())),
            kind => opt(
                if kind == ForwardType::Local {
                    "LocalForward"
                } else {
                    "RemoteForward"
                },
                Some(format!(
                    "{} {}",
                    f.bind_port,
                    forward_target(
                        f.target_host.as_deref().unwrap_or(""),
                        f.target_port.unwrap_or(0)
                    )
                )),
            ),
        }
    }
    opt(
        "StrictHostKeyChecking",
        o.strict_host_key_checking.map(|v| v.as_str().to_string()),
    );
    opt(
        "RemoteCommand",
        o.remote_command.as_deref().map(|c| c.trim().to_string()),
    );
    opt("RequestTTY", o.request_tty.map(|v| v.as_str().to_string()));
    for extra in o.extras.iter().flatten() {
        opt(&extra.keyword, Some(extra.value.trim().to_string()));
    }
    lines
}

/// Match the file's existing option indentation; default to two spaces.
fn detect_indent(doc: &ConfigDocument, file: &Path) -> String {
    for line in doc.files.get(file).into_iter().flatten() {
        let lead: String = line
            .chars()
            .take_while(|c| *c == ' ' || *c == '\t')
            .collect();
        if !lead.is_empty()
            && line[lead.len()..]
                .chars()
                .next()
                .is_some_and(|c| !c.is_whitespace())
        {
            return lead;
        }
    }
    "  ".to_string()
}

// ---------------------------------------------------------------------------
// File surgery
// ---------------------------------------------------------------------------

/// Config edits stay inside the root config's directory (normally ~/.ssh).
fn resolve_target_file(file: &Path, root_path: &Path) -> Result<PathBuf, Problem> {
    let resolved = lexical_absolute(file);
    let dir = root_path.parent().unwrap_or(Path::new("/"));
    if !is_within(&resolved, dir) {
        return Err(Problem::bad_request(format!(
            "config files must live under {}",
            path_string(dir)
        )));
    }
    Ok(resolved)
}

fn remove_block(files: &mut HashMap<PathBuf, Vec<String>>, block: &HostBlock) {
    let lines = files.entry(block.file.clone()).or_default();
    let start = block.comment_start.min(lines.len());
    lines.drain(start..block.end.min(lines.len()));
    // Collapse the blank seam the removal leaves behind.
    let blank = |line: Option<&String>| line.is_none_or(|l| l.trim().is_empty());
    if blank(lines.get(start)) && (start == 0 || blank(lines.get(start - 1))) && start < lines.len()
    {
        lines.remove(start);
    }
    while lines.last().is_some_and(|l| l.trim().is_empty()) {
        lines.pop();
    }
}

fn append_block(files: &mut HashMap<PathBuf, Vec<String>>, file: &Path, rendered: Vec<String>) {
    let lines = files.entry(file.to_path_buf()).or_default();
    if lines.last().is_some_and(|l| !l.trim().is_empty()) {
        lines.push(String::new());
    }
    lines.extend(rendered);
}

/// Reachable-from-root check; adds an Include to the root config when needed.
fn ensure_included(doc: &mut ConfigDocument, target_file: &Path) -> bool {
    if target_file == doc.root_path || doc.file_order.iter().any(|f| f == target_file) {
        return false;
    }
    let root_dir = doc
        .root_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_default();
    let rel = relative_inside(&root_dir, target_file);
    let root_lines = doc.files.entry(doc.root_path.clone()).or_default();
    // Insert before the first directive (appending could land inside the last Host block, where
    // Include has per-block semantics). Top placement is the conventional spot for Includes in
    // per-user configs.
    let at = root_lines
        .iter()
        .position(|l| {
            let t = l.trim();
            !t.is_empty() && !t.starts_with('#')
        })
        .unwrap_or(root_lines.len());
    root_lines.splice(
        at..at,
        [format!("Include {}", path_string(&rel)), String::new()],
    );
    true
}

/// Follow the final path component so an atomic rename does not replace a symlink.
fn resolve_write_target(file_path: &Path) -> Result<PathBuf, Problem> {
    let mut target = lexical_absolute(file_path);
    let mut seen = std::collections::HashSet::new();
    loop {
        if !seen.insert(target.clone()) {
            return Err(Problem::internal(format!(
                "symbolic link cycle while resolving {}",
                path_string(file_path)
            )));
        }
        match std::fs::symlink_metadata(&target) {
            Ok(meta) if meta.file_type().is_symlink() => {
                let link = std::fs::read_link(&target)?;
                let parent = target.parent().map(Path::to_path_buf).unwrap_or_default();
                target = lexical_absolute(&parent.join(link));
            }
            Ok(_) => return Ok(target),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(target),
            Err(err) => return Err(err.into()),
        }
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Atomically replaces `file_path` with `lines`, keeping its mode and a `.muxus.bak` copy.
pub(crate) fn write_config_file(file_path: &Path, lines: &[String]) -> Result<(), Problem> {
    if let Some(dir) = file_path.parent() {
        create_private_dir(dir)?;
    }
    let write_target = resolve_write_target(file_path)?;
    if let Some(dir) = write_target.parent() {
        create_private_dir(dir)?;
    }
    let mut mode = 0o600;
    let backup = PathBuf::from(format!("{}.muxus.bak", path_string(file_path)));
    if let Ok(meta) = std::fs::metadata(&write_target) {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            mode = meta.permissions().mode() & 0o777;
        }
        #[cfg(not(unix))]
        let _ = meta;
        if std::fs::copy(&write_target, &backup).is_ok() {
            set_mode(&backup, 0o600);
        }
    }
    let tmp = PathBuf::from(format!("{}.muxus.tmp", path_string(&write_target)));
    let text = if lines.is_empty() {
        String::new()
    } else {
        format!("{}\n", lines.join("\n"))
    };
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(mode);
        }
        let mut file = options.open(&tmp)?;
        file.write_all(text.as_bytes())?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    std::fs::rename(&tmp, &write_target)?;
    Ok(())
}

fn set_mode(path: &Path, mode: u32) {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode));
    }
    #[cfg(not(unix))]
    let _ = (path, mode);
}

#[cfg(test)]
mod tests;
