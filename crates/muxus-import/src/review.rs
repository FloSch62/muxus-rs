//! Everything the import dialog shows and decides that is not drawing
//! (`client/src/components/SessionImportDialog.tsx` and the per-vendor
//! `MobaXtermImportDialog.tsx` / `SecureCrtImportDialog.tsx`): the vendor
//! copy, file checks, the review selection, search, conflicts and the
//! messages around an import.

use std::collections::HashSet;
use std::path::Path;

use crate::mobaxterm::{
    MAX_MOBAXTERM_IMPORT_BYTES, mobaxterm_connections, parse_mobaxterm_sessions,
};
use crate::portable::PortableConnections;
use crate::restore::{ConflictStrategy, ExistingHosts, RestorePlan, RestoreResult, plan_restore};
use crate::securecrt::{
    MAX_SECURECRT_IMPORT_BYTES, parse_securecrt_sessions, securecrt_connections,
};
use crate::session::{
    ImportError, ImportedAuthMode, ImportedSession, ImportedSessionParseResult, ImportedSshStorage,
    SkippedImportedSession,
};

/// Skipped sessions are listed this many at a time.
pub const SKIPPED_SESSIONS_PER_PAGE: usize = 50;

/// The program sessions are imported from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImportVendor {
    MobaXterm,
    SecureCrt,
}

/// The "find local sessions" card of a vendor that has one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AutoSourceInfo {
    pub title: &'static str,
    pub description: &'static str,
}

impl ImportVendor {
    pub fn name(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => "MobaXterm",
            ImportVendor::SecureCrt => "SecureCRT",
        }
    }

    pub fn max_bytes(self) -> usize {
        match self {
            ImportVendor::MobaXterm => MAX_MOBAXTERM_IMPORT_BYTES,
            ImportVendor::SecureCrt => MAX_SECURECRT_IMPORT_BYTES,
        }
    }

    /// Subtitle before a file is chosen.
    pub fn idle_subtitle(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => {
                "Bring your saved SSH, RDP and VNC sessions and folders into Muxus."
            }
            ImportVendor::SecureCrt => {
                "Bring exported SSH and serial sessions and folders into Muxus."
            }
        }
    }

    pub fn source_intro(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => {
                "On Windows, Muxus can find bookmark data from the installed or portable edition. You can also choose an exported file on any platform."
            }
            ImportVendor::SecureCrt => {
                "In SecureCRT, use Tools → Export Settings, include Sessions, then choose the resulting XML file here."
            }
        }
    }

    /// The file card's title (`<vendor> file`).
    pub fn file_title(self) -> String {
        format!("{} file", self.name())
    }

    pub fn file_description(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => {
                "Choose MobaXterm.ini, .mxtsessions, .mobaconf or a text export."
            }
            ImportVendor::SecureCrt => "Choose an XML settings export created by SecureCRT.",
        }
    }

    /// The file picker's `accept` list as the TypeScript writes it.
    pub fn file_accept(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => ".ini,.mxtsessions,.mobaconf,.txt,text/plain",
            ImportVendor::SecureCrt => ".xml,text/xml,application/xml",
        }
    }

    /// File name extensions the picker offers, without dots.
    pub fn file_extensions(self) -> &'static [&'static str] {
        match self {
            ImportVendor::MobaXterm => &["ini", "mxtsessions", "mobaconf", "txt"],
            ImportVendor::SecureCrt => &["xml"],
        }
    }

    /// Shown above the session list.
    pub fn review_notice(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => {
                "Passwords are not copied, and private keys are referenced where they are, not copied. SSH gateways become jump hosts in a “MobaXterm jump hosts” folder. Muxus asks for credentials when you connect."
            }
            ImportVendor::SecureCrt => {
                "Passwords, private keys, embedded files and global settings are not copied. Muxus will ask for SSH credentials when needed."
            }
        }
    }

    /// Shown below the source cards.
    pub fn privacy_notice(self) -> &'static str {
        match self {
            ImportVendor::MobaXterm => {
                "Muxus only reads connection metadata. It never imports MobaXterm passwords or writes secrets into your SSH config."
            }
            ImportVendor::SecureCrt => {
                "Muxus reads SSH and serial connection metadata only. Local-shell sessions and unsupported or incomplete entries are skipped."
            }
        }
    }

    /// The local-discovery card; MobaXterm on Windows only.
    pub fn auto_source(self) -> Option<AutoSourceInfo> {
        match self {
            ImportVendor::MobaXterm if cfg!(windows) => Some(AutoSourceInfo {
                title: "Local MobaXterm",
                description: "Read bookmark names, hosts and folders from this Windows account.",
            }),
            _ => None,
        }
    }

    pub fn parse(self, text: &str) -> Result<ImportedSessionParseResult, ImportError> {
        match self {
            ImportVendor::MobaXterm => parse_mobaxterm_sessions(text),
            ImportVendor::SecureCrt => parse_securecrt_sessions(text),
        }
    }

    pub fn connections<'a>(
        self,
        sessions: impl IntoIterator<Item = &'a ImportedSession>,
        ssh_storage: ImportedSshStorage,
    ) -> PortableConnections {
        match self {
            ImportVendor::MobaXterm => mobaxterm_connections(sessions, ssh_storage),
            ImportVendor::SecureCrt => securecrt_connections(sessions, ssh_storage),
        }
    }

    /// Refuse a chosen file before reading it.
    pub fn check_file_size(self, bytes: u64) -> Result<(), ImportError> {
        if bytes > self.max_bytes() as u64 {
            return Err(ImportError(format!(
                "That {} file is larger than {} MB.",
                self.name(),
                format_megabytes(self.max_bytes())
            )));
        }
        Ok(())
    }

    /// When local discovery fails without a message of its own.
    pub fn detect_error(self) -> String {
        format!("Could not read local {} sessions.", self.name())
    }

    /// When reading the chosen file fails without a message of its own.
    pub fn read_error(self) -> String {
        format!("Could not read that {} file.", self.name())
    }

    /// When the import fails without a message of its own.
    pub fn import_error(self) -> String {
        format!("The {} sessions could not be imported.", self.name())
    }

    /// The success toast.
    pub fn import_complete_message(self, result: &RestoreResult) -> String {
        let summary = result.summary();
        if summary.is_empty() {
            format!("{} import complete.", self.name())
        } else {
            format!("{} import complete — {summary}.", self.name())
        }
    }
}

/// `File.text()`: UTF-8 with replacement characters, without a leading BOM.
pub fn decode_import_file(bytes: &[u8]) -> String {
    let bytes = bytes.strip_prefix(b"\xEF\xBB\xBF").unwrap_or(bytes);
    String::from_utf8_lossy(bytes).into_owned()
}

fn format_megabytes(bytes: usize) -> String {
    let megabytes = bytes as f64 / (1024.0 * 1024.0);
    // Math.round: halves go up.
    format!("{}", (megabytes + 0.5).floor())
}

/// The review step of the import dialog for one parsed file.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportReview {
    vendor: ImportVendor,
    source: String,
    parsed: ImportedSessionParseResult,
    selected: HashSet<String>,
    /// What to do with sessions that already exist; asked only when some do.
    pub conflicts: ConflictStrategy,
    /// Where SSH hosts go; the dialog starts with Muxus app data.
    pub ssh_storage: ImportedSshStorage,
    /// The session filter as typed.
    pub search: String,
}

impl ImportReview {
    /// Parse `content` read from `source` (a file name or the discovery
    /// origin) and select every session.
    pub fn from_content(
        vendor: ImportVendor,
        content: &str,
        source: impl Into<String>,
    ) -> Result<Self, ImportError> {
        Ok(Self::new(vendor, source, vendor.parse(content)?))
    }

    /// Read a chosen file the way the dialog does: refuse it by size first,
    /// then decode and parse it off the async threads. The review's source is
    /// the file's name.
    pub async fn read_file(vendor: ImportVendor, path: &Path) -> Result<Self, ImportError> {
        let metadata = tokio::fs::metadata(path)
            .await
            .map_err(|_| ImportError(vendor.read_error()))?;
        vendor.check_file_size(metadata.len())?;
        let bytes = tokio::fs::read(path)
            .await
            .map_err(|_| ImportError(vendor.read_error()))?;
        let source = path
            .file_name()
            .map_or_else(|| path.to_string_lossy(), |name| name.to_string_lossy())
            .into_owned();
        tokio::task::spawn_blocking(move || {
            Self::from_content(vendor, &decode_import_file(&bytes), source)
        })
        .await
        .map_err(|_| ImportError(vendor.read_error()))?
    }

    /// Find this Windows account's MobaXterm bookmarks and review them.
    pub async fn detect_local_mobaxterm() -> Result<Self, ImportError> {
        let found = crate::mobaxterm_local::read_local_mobaxterm_sessions().await?;
        tokio::task::spawn_blocking(move || {
            Self::from_content(ImportVendor::MobaXterm, &found.content, found.source)
        })
        .await
        .map_err(|_| ImportError(ImportVendor::MobaXterm.detect_error()))?
    }

    pub fn new(
        vendor: ImportVendor,
        source: impl Into<String>,
        parsed: ImportedSessionParseResult,
    ) -> Self {
        let selected = parsed
            .sessions
            .iter()
            .map(|session| session.id().to_owned())
            .collect();
        Self {
            vendor,
            source: source.into(),
            parsed,
            selected,
            conflicts: ConflictStrategy::Keep,
            ssh_storage: ImportedSshStorage::Muxus,
            search: String::new(),
        }
    }

    pub fn vendor(&self) -> ImportVendor {
        self.vendor
    }

    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn parsed(&self) -> &ImportedSessionParseResult {
        &self.parsed
    }

    pub fn sessions(&self) -> &[ImportedSession] {
        &self.parsed.sessions
    }

    /// `<n> supported sessions from <source>`.
    pub fn subtitle(&self) -> String {
        format!(
            "{} supported sessions from {}",
            self.parsed.sessions.len(),
            self.source
        )
    }

    /// The warning above the skipped list, when anything was skipped.
    pub fn skipped_notice(&self) -> Option<String> {
        match self.parsed.ignored_count {
            0 => None,
            1 => Some("1 unsupported or incomplete session was skipped.".to_owned()),
            count => Some(format!(
                "{count} unsupported or incomplete sessions were skipped."
            )),
        }
    }

    /// One page of the skipped list (`page` is clamped).
    pub fn skipped_page(&self, page: usize) -> SkippedPage<'_> {
        SkippedPage::new(&self.parsed.skipped_sessions, page)
    }

    /// Whether to ask where SSH hosts go. Jump hosts of RDP/VNC sessions are
    /// SSH hosts too, so they follow this choice.
    pub fn has_ssh_hosts(&self) -> bool {
        self.parsed.sessions.iter().any(|session| match session {
            ImportedSession::Ssh(_) => true,
            ImportedSession::Rdp(desktop) | ImportedSession::Vnc(desktop) => {
                desktop.jump_host.is_some()
            }
            ImportedSession::Serial(_) => false,
        })
    }

    /// The caption under the storage choice.
    pub fn storage_caption(&self) -> &'static str {
        match self.ssh_storage {
            ImportedSshStorage::Muxus => {
                "SSH connection settings stay in the Muxus database; ssh_config is not changed."
            }
            ImportedSshStorage::Openssh => {
                "Creates standard Host blocks that also work with ssh in any terminal."
            }
        }
    }

    /// The sessions the filter lets through, in file order.
    pub fn filtered_sessions(&self) -> Vec<&ImportedSession> {
        let needle = muxus_core::js::js_trim(&self.search).to_lowercase();
        if needle.is_empty() {
            return self.parsed.sessions.iter().collect();
        }
        self.parsed
            .sessions
            .iter()
            .filter(|session| {
                searchable_session_values(session)
                    .iter()
                    .flatten()
                    .any(|value| value.to_lowercase().contains(&needle))
            })
            .collect()
    }

    pub fn is_selected(&self, id: &str) -> bool {
        self.selected.contains(id)
    }

    pub fn toggle(&mut self, id: &str) {
        if !self.selected.remove(id) {
            self.selected.insert(id.to_owned());
        }
    }

    pub fn selected_count(&self) -> usize {
        self.selected.len()
    }

    /// Whether every session is selected (the button then clears).
    pub fn all_selected(&self) -> bool {
        self.selected.len() == self.parsed.sessions.len()
    }

    /// "Select all" or "Clear selection".
    pub fn toggle_all_label(&self) -> &'static str {
        if self.all_selected() {
            "Clear selection"
        } else {
            "Select all"
        }
    }

    pub fn toggle_all(&mut self) {
        if self.all_selected() {
            self.selected.clear();
        } else {
            self.selected = self
                .parsed
                .sessions
                .iter()
                .map(|session| session.id().to_owned())
                .collect();
        }
    }

    /// `Import <n> session(s)`; disabled at zero.
    pub fn import_label(&self) -> String {
        let count = self.selected_count();
        format!(
            "Import {count} {}",
            if count == 1 { "session" } else { "sessions" }
        )
    }

    /// Whether importing `session` would meet a host Muxus already has.
    pub fn is_conflict(&self, session: &ImportedSession, existing: &ExistingHosts) -> bool {
        let converted = self
            .vendor
            .connections(std::iter::once(session), self.ssh_storage);
        if let Some(profile) = converted.saved_hosts.first() {
            return existing.has_profile(&profile.id);
        }
        converted
            .ssh_hosts
            .first()
            .is_some_and(|host| existing.has_alias(&host.alias))
    }

    /// Sessions that already exist; the keep/replace choice is shown when any do.
    pub fn conflicting_count(&self, existing: &ExistingHosts) -> usize {
        self.parsed
            .sessions
            .iter()
            .filter(|session| self.is_conflict(session, existing))
            .count()
    }

    /// `Keep existing (<n>)`.
    pub fn keep_label(&self, existing: &ExistingHosts) -> String {
        format!("Keep existing ({})", self.conflicting_count(existing))
    }

    /// The selected sessions, in file order.
    pub fn included_sessions(&self) -> Vec<&ImportedSession> {
        self.parsed
            .sessions
            .iter()
            .filter(|session| self.selected.contains(session.id()))
            .collect()
    }

    /// The portable connections of the selected sessions.
    pub fn connections(&self) -> PortableConnections {
        self.vendor
            .connections(self.included_sessions(), self.ssh_storage)
    }

    /// The writes the import performs; `None` when nothing is selected.
    pub fn plan(&self, existing: &ExistingHosts) -> Option<RestorePlan> {
        if self.selected.is_empty() || self.included_sessions().is_empty() {
            return None;
        }
        Some(plan_restore(&self.connections(), existing, self.conflicts))
    }
}

/// What the session filter matches against.
pub fn searchable_session_values(session: &ImportedSession) -> Vec<Option<String>> {
    match session {
        ImportedSession::Serial(serial) => vec![
            Some(serial.name.clone()),
            Some(serial.path.clone()),
            Some(serial.baud_rate.to_string()),
            serial.folder.clone(),
        ],
        ImportedSession::Ssh(ssh) => vec![
            Some(ssh.name.clone()),
            Some(ssh.alias.clone()),
            Some(ssh.host.clone()),
            ssh.username.clone(),
            ssh.folder.clone(),
            ssh.jump_host.as_ref().map(|jump| jump.name.clone()),
        ],
        ImportedSession::Rdp(desktop) | ImportedSession::Vnc(desktop) => vec![
            Some(desktop.name.clone()),
            Some(session.kind().as_str().to_uppercase()),
            Some(desktop.host.clone()),
            desktop.username.clone(),
            desktop.folder.clone(),
            desktop.jump_host.as_ref().map(|jump| jump.name.clone()),
        ],
    }
}

/// The chip beside a non-SSH session's name.
pub fn session_kind_label(session: &ImportedSession) -> Option<&'static str> {
    match session {
        ImportedSession::Ssh(_) => None,
        ImportedSession::Serial(_) => Some("Serial"),
        ImportedSession::Rdp(_) => Some("RDP"),
        ImportedSession::Vnc(_) => Some("VNC"),
    }
}

/// The secondary line of a session row.
pub fn session_details(session: &ImportedSession) -> String {
    let address = |username: &Option<String>, host: &str, port: u16| {
        format!(
            "{}{host}:{port}",
            username
                .as_deref()
                .map(|user| format!("{user}@"))
                .unwrap_or_default()
        )
    };
    match session {
        ImportedSession::Ssh(ssh) => {
            let mut parts = vec![address(&ssh.username, &ssh.host, ssh.port)];
            parts.push(match &ssh.identity_file {
                Some(file) => format!("key {}", file.rsplit('/').next().unwrap_or(file)),
                None if ssh.auth_mode == ImportedAuthMode::Password => "password prompt".to_owned(),
                None => "SSH key / agent".to_owned(),
            });
            if let Some(jump) = &ssh.jump_host {
                parts.push(format!("via {}", jump.name));
            }
            if let Some(command) = &ssh.remote_command {
                parts.push(format!("runs {command}"));
            }
            parts.join(" · ")
        }
        ImportedSession::Rdp(desktop) | ImportedSession::Vnc(desktop) => {
            let mut parts = vec![address(&desktop.username, &desktop.host, desktop.port)];
            if let Some(jump) = &desktop.jump_host {
                parts.push(format!("via {}", jump.name));
            }
            parts.join(" · ")
        }
        ImportedSession::Serial(serial) => {
            use muxus_core::protocol::{SerialFlowControl, SerialParity, StopBits};
            let parity = match serial.parity {
                SerialParity::None => "N",
                SerialParity::Even => "E",
                SerialParity::Odd => "O",
                SerialParity::Mark => "M",
                SerialParity::Space => "S",
            };
            let stop_bits = match serial.stop_bits {
                StopBits::One => "1",
                StopBits::OnePointFive => "1.5",
                StopBits::Two => "2",
            };
            let flow = match serial.flow_control {
                SerialFlowControl::Hardware => "hardware flow",
                SerialFlowControl::Software => "XON/XOFF",
                SerialFlowControl::None => "no flow control",
            };
            format!(
                "{} · {} baud · {}{parity}{stop_bits} · {flow}",
                serial.path, serial.baud_rate, serial.data_bits
            )
        }
    }
}

/// A skipped session as listed: `folder/name`.
pub fn skipped_session_name(session: &SkippedImportedSession) -> String {
    match &session.folder {
        Some(folder) => format!("{folder}/{}", session.name),
        None => session.name.clone(),
    }
}

/// One page of skipped sessions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedPage<'a> {
    pub sessions: &'a [SkippedImportedSession],
    /// Zero-based, after clamping.
    pub page: usize,
    pub page_count: usize,
    /// Zero-based index of the first session shown.
    pub start: usize,
    /// One past the last session shown.
    pub end: usize,
    pub total: usize,
}

impl<'a> SkippedPage<'a> {
    pub fn new(all: &'a [SkippedImportedSession], page: usize) -> Self {
        let page_count = all.len().div_ceil(SKIPPED_SESSIONS_PER_PAGE);
        let page = page.min(page_count.saturating_sub(1));
        let start = page * SKIPPED_SESSIONS_PER_PAGE;
        let end = (start + SKIPPED_SESSIONS_PER_PAGE).min(all.len());
        Self {
            sessions: &all[start..end],
            page,
            page_count,
            start,
            end,
            total: all.len(),
        }
    }

    /// Whether the pager shows at all.
    pub fn has_pager(&self) -> bool {
        self.page_count > 1
    }

    pub fn has_previous(&self) -> bool {
        self.page > 0
    }

    pub fn has_next(&self) -> bool {
        self.page + 1 < self.page_count
    }

    /// `Showing 1–50 of 120`.
    pub fn label(&self) -> String {
        format!("Showing {}–{} of {}", self.start + 1, self.end, self.total)
    }
}
