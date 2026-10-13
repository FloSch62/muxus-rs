//! The non-visual parts of the Backup & data section and its restore review
//! dialog (`client/src/components/DataTransferSection.tsx`): the texts it
//! computes, the last-backup timestamp, reading a chosen file and the
//! categories a restore offers.

use std::path::Path;

use chrono::{DateTime, Datelike, Local};
use serde_json::Value;

use muxus_session::Backend;

use super::validate::js_date_parse;
use super::{
    LAST_BACKUP_KEY, MAX_TRANSFER_FILE_BYTES, RestoreResult, TransferDocument, TransferError,
    parse_transfer_document,
};

/// Toast after a backup was saved.
pub const BACKUP_SAVED: &str = "Backup saved.";
/// Toast when a backup failed without a message of its own.
pub const BACKUP_FAILED: &str = "Could not create the backup.";
/// Toast after the OpenSSH export was saved.
pub const OPENSSH_EXPORT_SAVED: &str = "OpenSSH config export saved.";
/// Toast when the export failed without a message of its own.
pub const OPENSSH_EXPORT_FAILED: &str = "Could not export the connections.";
/// Toast for a chosen or dropped file over [`MAX_TRANSFER_FILE_BYTES`].
pub const TRANSFER_FILE_TOO_LARGE: &str = "That file is too large for a Muxus settings transfer.";
/// Toast when a chosen file cannot be read at all.
pub const TRANSFER_FILE_UNREADABLE: &str = "Could not read that file.";

/// The counts the section shows next to "Back up your setup".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DataSummary {
    pub connections: usize,
    pub tunnels: usize,
}

/// `3 connections`, `1 tunnel`.
pub fn plural(count: usize, noun: &str) -> String {
    format!("{count} {noun}{}", if count == 1 { "" } else { "s" })
}

/// What the backup row says it covers, before ". One portable file, ready
/// for another Muxus install."
pub fn summary_line(summary: Option<&DataSummary>, loading: bool) -> String {
    if loading {
        return "Reading your Muxus data…".to_string();
    }
    match summary {
        Some(summary) => [
            plural(summary.connections, "connection"),
            plural(summary.tunnels, "tunnel"),
        ]
        .join("  ·  "),
        None => "Settings, connections, tunnels and logging policies".to_string(),
    }
}

/// The second line of the backup row.
pub fn last_backup_line(last_backup: Option<&str>) -> String {
    match last_backup {
        Some(value) => format!("Last backup: {}.", friendly_date(value)),
        None => "No backup yet.".to_string(),
    }
}

/// A date the way the section shows it: the time alone for today, otherwise
/// the date and time (`Intl.DateTimeFormat` in the en-US style).
pub fn friendly_date(value: &str) -> String {
    friendly_date_at(value, Local::now())
}

pub(super) fn friendly_date_at(value: &str, now: DateTime<Local>) -> String {
    let Some(date) = js_date_parse(value) else {
        return value.to_string();
    };
    let date = date.with_timezone(&Local);
    let same_day =
        date.year() == now.year() && date.month() == now.month() && date.day() == now.day();
    if same_day {
        date.format("%I:%M %p").to_string()
    } else {
        date.format("%b %-d, %Y, %I:%M %p").to_string()
    }
}

/// When the last backup was made on this machine, if a valid time is stored.
pub async fn read_last_backup(backend: &Backend) -> Option<String> {
    backend
        .client_state_item(LAST_BACKUP_KEY)
        .await
        .ok()
        .flatten()
        .filter(|value| js_date_parse(value).is_some())
}

/// Remember a backup's `createdAt` as the last backup.
pub async fn remember_backup(backend: &Backend, created_at: &str) -> Result<(), TransferError> {
    backend
        .set_client_state_item(LAST_BACKUP_KEY, Some(created_at))
        .await?;
    Ok(())
}

/// `muxus-backup-YYYY-MM-DD.muxus` (UTC date), saved as `application/json`.
pub fn dated_transfer_filename() -> String {
    format!("muxus-backup-{}.muxus", utc_date())
}

/// `muxus-openssh-connections-YYYY-MM-DD.conf` (UTC date), saved as `text/plain`.
pub fn openssh_export_filename() -> String {
    format!("muxus-openssh-connections-{}.conf", utc_date())
}

fn utc_date() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// A backup chosen or dropped for restoring, waiting for its review.
#[derive(Debug, Clone, PartialEq)]
pub struct PendingFile {
    pub document: TransferDocument,
    pub filename: String,
}

/// Read a dropped or chosen file's contents. The file picker offers
/// `.muxus` and `application/json`.
pub fn read_transfer_file(filename: &str, bytes: &[u8]) -> Result<PendingFile, TransferError> {
    if bytes.len() as u64 > MAX_TRANSFER_FILE_BYTES {
        return Err(TransferError::new(TRANSFER_FILE_TOO_LARGE));
    }
    // `File.text()` decodes as UTF-8, replacing what is not.
    let text = String::from_utf8_lossy(bytes);
    Ok(PendingFile {
        document: parse_transfer_document(&text)?,
        filename: filename.to_string(),
    })
}

/// Read a file from disk, refusing one over the size limit before reading it.
pub fn load_transfer_file(path: &Path) -> Result<PendingFile, TransferError> {
    let unreadable = |_| TransferError::new(TRANSFER_FILE_UNREADABLE);
    let size = std::fs::metadata(path).map_err(unreadable)?.len();
    if size > MAX_TRANSFER_FILE_BYTES {
        return Err(TransferError::new(TRANSFER_FILE_TOO_LARGE));
    }
    let bytes = std::fs::read(path).map_err(unreadable)?;
    let filename = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    read_transfer_file(&filename, &bytes)
}

/// One part of a backup the review dialog lets the user restore or leave.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RestoreCategory {
    Preferences,
    Connections,
    Tunnels,
    Logging,
}

impl RestoreCategory {
    pub const ALL: [RestoreCategory; 4] = [
        RestoreCategory::Preferences,
        RestoreCategory::Connections,
        RestoreCategory::Tunnels,
        RestoreCategory::Logging,
    ];
}

/// A row of the review dialog's category list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreCategoryRow {
    pub category: RestoreCategory,
    /// MUI icon name (`SettingsOutlined`, …).
    pub icon: &'static str,
    pub label: &'static str,
    pub detail: &'static str,
    /// The chip at the end of the row; app settings show none.
    pub count: Option<usize>,
}

/// The four rows of the review dialog, in order.
pub fn restore_categories(document: &TransferDocument) -> [RestoreCategoryRow; 4] {
    [
        RestoreCategoryRow {
            category: RestoreCategory::Preferences,
            icon: "SettingsOutlined",
            label: "App settings",
            detail: "Appearance, terminal, behavior and global highlighting",
            count: None,
        },
        RestoreCategoryRow {
            category: RestoreCategory::Connections,
            icon: "DnsOutlined",
            label: "Connections",
            detail: "SSH, Telnet, serial, RDP and VNC profiles",
            count: Some(connection_count(document)),
        },
        RestoreCategoryRow {
            category: RestoreCategory::Tunnels,
            icon: "AltRouteOutlined",
            label: "Tunnels",
            detail: "Saved port-forwarding definitions",
            count: Some(document.tunnels().len()),
        },
        RestoreCategoryRow {
            category: RestoreCategory::Logging,
            icon: "HistoryOutlined",
            label: "Logging settings",
            detail: "Policies and retention limits; no recordings",
            count: Some(document.logging_policies().len()),
        },
    ]
}

pub fn connection_count(document: &TransferDocument) -> usize {
    document.ssh_hosts().len() + document.saved_hosts().len()
}

/// Connections in the file that carry a ProxyCommand, which runs as a local
/// shell command once such a connection is opened.
pub fn proxy_command_count(document: &TransferDocument) -> usize {
    let has_command = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|command| !muxus_core::js::js_trim(command).is_empty())
    };
    let ssh = document
        .ssh_hosts()
        .iter()
        .filter(|host| {
            has_command(
                host.get("options")
                    .and_then(|options| options.get("proxyCommand")),
            )
        })
        .count();
    let saved = document
        .saved_hosts()
        .iter()
        .filter(|host| {
            host.get("profile").is_some_and(|profile| {
                profile.get("kind").and_then(Value::as_str) == Some("ssh")
                    && has_command(profile.get("proxyCommand"))
            })
        })
        .count();
    ssh + saved
}

/// The warning shown while connections are selected and some carry a ProxyCommand.
pub fn proxy_command_warning(count: usize) -> String {
    format!(
        "{count} {} a ProxyCommand. Muxus does not run it during restore, but it will run as a \
         local shell command when that connection is opened. Restore only backups you trust.",
        if count == 1 {
            "connection contains"
        } else {
            "connections contain"
        }
    )
}

/// The caption under "Restore this backup?": file, date and the writing version.
pub fn review_subtitle(pending: &PendingFile) -> String {
    let mut subtitle = format!(
        "{} · {}",
        pending.filename,
        friendly_date(pending.document.created_at())
    );
    if let Some(version) = pending
        .document
        .app_version()
        .filter(|version| !version.is_empty())
    {
        subtitle.push_str(&format!(" · Muxus {version}"));
    }
    subtitle
}

/// The success toast: `Restore complete — 2 added · 1 kept.`
pub fn restore_complete_message(result: &RestoreResult) -> String {
    let parts: Vec<String> = [
        (result.added, "added"),
        (result.updated, "updated"),
        (result.skipped, "kept"),
    ]
    .into_iter()
    .filter(|(count, _)| *count > 0)
    .map(|(count, label)| format!("{count} {label}"))
    .collect();
    if parts.is_empty() {
        "Restore complete.".to_string()
    } else {
        format!("Restore complete — {}.", parts.join(" · "))
    }
}

/// The error alert of the review dialog after a restore stopped part way.
pub fn restore_failed_message(error: &str) -> String {
    format!(
        "{error} Some items may already have been restored; it is safe to review the file and \
         try again."
    )
}
