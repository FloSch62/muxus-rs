//! Hosts: Muxus metadata for OpenSSH aliases, Muxus-owned saved hosts,
//! sidebar groups and order, saved management requests and pinned remote
//! desktop identities.

use std::collections::HashSet;

use muxus_core::api::{
    HostKeywordHighlightConfig, LoginSequence, ManagedHostRef, OpenSshMetadataPatch,
    OpenSshProfileMetadata, SavedHostKind, SavedHostProfile, SavedHostProfileInput,
};
use muxus_core::js::Patch;
use muxus_core::management::{
    ManagementProtocol, SavedManagementRequest, SavedManagementRequestInput,
};
use muxus_core::protocol::SavedHostSessionProfile;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Map, Value};

use super::{Database, DbError, DbResult, assert_secret_free, optional_string, require_non_empty};
use crate::ids::nanoid;

/// Kinds stored as Muxus-owned saved hosts (everything but OpenSSH metadata rows).
const SAVED_HOST_KINDS_SQL: &str = "('ssh', 'serial', 'telnet', 'rdp', 'vnc', 'gnmi', 'netconf')";

const METADATA_BY_ALIAS_SQL: &str = "
      SELECT
        profiles.id,
        profiles.name,
        profiles.ssh_alias,
        profiles.group_id,
        profiles.sort_order,
        profiles.color,
        profiles.icon,
        profiles.terminal_scheme,
        profiles.terminal_font_color,
        profiles.terminal_background_color,
        profiles.keyword_highlights_json,
        profiles.command_button_group,
        profiles.paste_line_delay_ms,
        profiles.paste_char_delay_ms,
        profiles.disable_sftp,
        profiles.console_compatibility,
        profiles.login_sequence_json,
        profiles.last_connected_at,
        profiles.connect_count,
        groups.name AS group_name
      FROM connection_profiles AS profiles
      LEFT JOIN connection_groups AS groups ON groups.id = profiles.group_id
      WHERE profiles.kind = 'openssh' AND profiles.ssh_alias = ?
    ";

fn saved_host_sql(filter: &str) -> String {
    format!(
        "
        SELECT profiles.*, groups.name AS group_name
        FROM connection_profiles AS profiles
        LEFT JOIN connection_groups AS groups ON groups.id = profiles.group_id
        WHERE {filter} AND profiles.kind IN {SAVED_HOST_KINDS_SQL}
      "
    )
}

/// The metadata columns shared by OpenSSH rows and saved hosts.
#[derive(Debug, Clone)]
struct MetadataRow {
    id: String,
    name: String,
    ssh_alias: Option<String>,
    group_id: Option<String>,
    sort_order: Option<i64>,
    color: Option<String>,
    icon: Option<String>,
    terminal_scheme: Option<String>,
    terminal_font_color: Option<String>,
    terminal_background_color: Option<String>,
    keyword_highlights_json: Option<String>,
    command_button_group: Option<String>,
    paste_line_delay_ms: Option<i64>,
    paste_char_delay_ms: Option<i64>,
    disable_sftp: bool,
    console_compatibility: bool,
    login_sequence_json: Option<String>,
    last_connected_at: Option<String>,
    connect_count: i64,
    group_name: Option<String>,
}

fn metadata_row(row: &Row<'_>) -> rusqlite::Result<MetadataRow> {
    Ok(MetadataRow {
        id: row.get("id")?,
        name: row.get("name")?,
        ssh_alias: row.get("ssh_alias")?,
        group_id: row.get("group_id")?,
        sort_order: row.get("sort_order")?,
        color: row.get("color")?,
        icon: row.get("icon")?,
        terminal_scheme: row.get("terminal_scheme")?,
        terminal_font_color: row.get("terminal_font_color")?,
        terminal_background_color: row.get("terminal_background_color")?,
        keyword_highlights_json: row.get("keyword_highlights_json")?,
        command_button_group: row.get("command_button_group")?,
        paste_line_delay_ms: row.get("paste_line_delay_ms")?,
        paste_char_delay_ms: row.get("paste_char_delay_ms")?,
        disable_sftp: row.get::<_, i64>("disable_sftp")? == 1,
        console_compatibility: row.get::<_, i64>("console_compatibility")? == 1,
        login_sequence_json: row.get("login_sequence_json")?,
        last_connected_at: row.get("last_connected_at")?,
        connect_count: row.get("connect_count")?,
        group_name: row.get("group_name")?,
    })
}

impl MetadataRow {
    /// `metadataFromRow` (with a display name) / the metadata of `savedHostFromRow`.
    fn metadata(&self, with_display_name: bool) -> OpenSshProfileMetadata {
        OpenSshProfileMetadata {
            profile_id: self.id.clone(),
            sort_order: self.sort_order,
            display_name: if with_display_name && Some(&self.name) != self.ssh_alias.as_ref() {
                Some(self.name.clone())
            } else {
                None
            },
            group: optional_string(self.group_name.clone()),
            color: optional_string(self.color.clone()),
            icon: optional_string(self.icon.clone()),
            terminal_scheme: optional_string(self.terminal_scheme.clone()),
            terminal_font_color: optional_string(self.terminal_font_color.clone()),
            terminal_background_color: optional_string(self.terminal_background_color.clone()),
            keyword_highlights: keyword_highlights_from_json(
                self.keyword_highlights_json.as_deref(),
            ),
            command_button_group: optional_string(self.command_button_group.clone()),
            paste_line_delay_ms: self
                .paste_line_delay_ms
                .map(|value| value.clamp(0, i64::from(u32::MAX)) as u32),
            paste_char_delay_ms: self
                .paste_char_delay_ms
                .map(|value| value.clamp(0, i64::from(u32::MAX)) as u32),
            disable_sftp: self.disable_sftp.then_some(true),
            console_compatibility: self.console_compatibility.then_some(true),
            login_sequence: login_sequence_from_json(self.login_sequence_json.as_deref()),
            last_connected_at: optional_string(self.last_connected_at.clone()),
            connect_count: self.connect_count.max(0) as u64,
        }
    }
}

/// Stored highlighting with the TypeScript's shape checks; anything
/// unreadable counts as none set.
fn keyword_highlights_from_json(value: Option<&str>) -> Option<HostKeywordHighlightConfig> {
    let parsed: Value = serde_json::from_str(value?).ok()?;
    let object = parsed.as_object()?;
    if !object.get("inheritGlobal").is_some_and(Value::is_boolean)
        || !object.get("rules").is_some_and(Value::is_array)
    {
        return None;
    }
    if let Some(profile_id) = object.get("profileId") {
        let valid = profile_id
            .as_str()
            .is_some_and(|id| !id.is_empty() && muxus_core::js::utf16_len(id) <= 200);
        if !valid {
            return None;
        }
    }
    serde_json::from_value(parsed).ok()
}

/// Stored sequences were validated when written; anything unreadable counts as none set.
pub(super) fn login_sequence_from_json(value: Option<&str>) -> Option<LoginSequence> {
    let parsed: Value = serde_json::from_str(value?).ok()?;
    if !parsed.get("steps").is_some_and(Value::is_array) {
        return None;
    }
    serde_json::from_value(parsed).ok()
}

/// A metadata patch's sequence as the column value: `Keep` keeps the current one.
pub(super) fn login_sequence_column(
    patch: &Patch<LoginSequence>,
    current: Option<String>,
) -> DbResult<Option<String>> {
    match patch {
        Patch::Keep => Ok(current),
        Patch::Clear => Ok(None),
        Patch::Set(sequence) => {
            let value = serde_json::to_value(sequence)?;
            assert_secret_free(&value, "loginSequence")?;
            Ok(Some(serde_json::to_string(sequence)?))
        }
    }
}

fn patched<T: Clone>(patch: &Patch<T>, current: Option<T>) -> Option<T> {
    match patch {
        Patch::Keep => current,
        Patch::Clear => None,
        Patch::Set(value) => Some(value.clone()),
    }
}

/// A saved host or an OpenSSH alias, for lookups that accept either.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostRef<'a> {
    Alias(&'a str),
    ProfileId(&'a str),
}

/// A host's own login sequence and the folder it lives in.
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HostLoginSequence {
    pub login_sequence: Option<LoginSequence>,
    pub group: Option<String>,
}

/// The kinds `createNativeConnection` accepts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NativeConnectionKind {
    Ssh,
    Local,
    Serial,
    Telnet,
    Rdp,
    Vnc,
}

impl NativeConnectionKind {
    fn as_str(self) -> &'static str {
        match self {
            NativeConnectionKind::Ssh => "ssh",
            NativeConnectionKind::Local => "local",
            NativeConnectionKind::Serial => "serial",
            NativeConnectionKind::Telnet => "telnet",
            NativeConnectionKind::Rdp => "rdp",
            NativeConnectionKind::Vnc => "vnc",
        }
    }
}

/// A raw native connection row (`createNativeConnection`).
#[derive(Debug, Clone, PartialEq)]
pub struct NativeConnectionInput {
    pub kind: NativeConnectionKind,
    pub name: String,
    pub config: Value,
    pub credential_ref_id: Option<String>,
}

/// The pinned RDP certificate or VNC server key for a host.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct DesktopIdentity {
    pub fingerprint: String,
    /// Describes what was pinned.
    pub subject: String,
}

fn metadata_by_alias(conn: &Connection, alias: &str) -> DbResult<Option<MetadataRow>> {
    Ok(conn
        .query_row(METADATA_BY_ALIAS_SQL, [alias], metadata_row)
        .optional()?)
}

fn ensure_open_ssh_profile(conn: &Connection, alias: &str) -> DbResult<MetadataRow> {
    if let Some(existing) = metadata_by_alias(conn, alias)? {
        return Ok(existing);
    }
    conn.execute(
        "
        INSERT INTO connection_profiles(id, kind, name, ssh_alias)
        VALUES (?, 'openssh', ?, ?)
      ",
        params![nanoid(), alias, alias],
    )?;
    metadata_by_alias(conn, alias)?
        .ok_or_else(|| DbError::NotFound("OpenSSH profile not found".into()))
}

/// Reuse group names case-insensitively so typing "work" and "Work" cannot
/// silently create two visually indistinguishable sidebar groups. A spelling
/// that differs only by case updates the row instead of being discarded, so
/// renaming a folder to fix its capitalization actually takes effect.
fn group_id_for_name(conn: &Connection, name: Option<&str>) -> DbResult<Option<String>> {
    let Some(normalized) = name
        .map(muxus_core::js::js_trim)
        .filter(|name| !name.is_empty())
    else {
        return Ok(None);
    };
    let existing: Option<(String, String)> = conn
        .query_row(
            "SELECT id, name FROM connection_groups WHERE name = ? COLLATE NOCASE ORDER BY created_at LIMIT 1",
            [normalized],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    if let Some((id, existing_name)) = existing {
        if existing_name != normalized {
            conn.execute(
                "UPDATE connection_groups SET name = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                params![normalized, id],
            )?;
        }
        return Ok(Some(id));
    }
    let id = nanoid();
    conn.execute(
        "INSERT INTO connection_groups(id, name) VALUES (?, ?)",
        params![id, normalized],
    )?;
    Ok(Some(id))
}

/// Write every metadata column of one row from a patch over its current values.
fn apply_metadata_patch(
    conn: &Connection,
    current: &MetadataRow,
    name: &str,
    patch: &OpenSshMetadataPatch,
) -> DbResult<()> {
    let group_id = match &patch.group {
        Patch::Keep => current.group_id.clone(),
        Patch::Clear => group_id_for_name(conn, None)?,
        Patch::Set(group) => group_id_for_name(conn, Some(group))?,
    };
    let keyword_highlights = match &patch.keyword_highlights {
        Patch::Keep => current.keyword_highlights_json.clone(),
        Patch::Clear => None,
        Patch::Set(config) => Some(serde_json::to_string(config)?),
    };
    conn.execute(
        "
        UPDATE connection_profiles
        SET name = ?,
            group_id = ?,
            color = ?,
            icon = ?,
            terminal_scheme = ?,
            terminal_font_color = ?,
            terminal_background_color = ?,
            keyword_highlights_json = ?,
            command_button_group = ?,
            paste_line_delay_ms = ?,
            paste_char_delay_ms = ?,
            disable_sftp = ?,
            console_compatibility = ?,
            login_sequence_json = ?,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
        params![
            name,
            group_id,
            patched(&patch.color, current.color.clone()),
            patched(&patch.icon, current.icon.clone()),
            patched(&patch.terminal_scheme, current.terminal_scheme.clone()),
            patched(
                &patch.terminal_font_color,
                current.terminal_font_color.clone()
            ),
            patched(
                &patch.terminal_background_color,
                current.terminal_background_color.clone()
            ),
            keyword_highlights,
            patched(
                &patch.command_button_group,
                current.command_button_group.clone()
            ),
            match patch.paste_line_delay_ms {
                Patch::Keep => current.paste_line_delay_ms,
                Patch::Clear => None,
                Patch::Set(value) => Some(i64::from(value)),
            },
            match patch.paste_char_delay_ms {
                Patch::Keep => current.paste_char_delay_ms,
                Patch::Clear => None,
                Patch::Set(value) => Some(i64::from(value)),
            },
            patch.disable_sftp.unwrap_or(current.disable_sftp),
            patch
                .console_compatibility
                .unwrap_or(current.console_compatibility),
            login_sequence_column(&patch.login_sequence, current.login_sequence_json.clone())?,
            current.id,
        ],
    )?;
    Ok(())
}

/// The stored configuration of a saved host: the profile without its kind
/// and id, in the profile's own field order.
fn saved_host_config(profile: &SavedHostSessionProfile) -> DbResult<Map<String, Value>> {
    let Value::Object(mut map) = serde_json::to_value(profile)? else {
        return Err(DbError::Invalid("profile must be an object".into()));
    };
    map.shift_remove("kind");
    map.shift_remove("profileId");
    Ok(map)
}

/// `savedHostFromRow`: `{ kind, ...config, profileId: id }`.
fn saved_host_from_row(row: &Row<'_>) -> rusqlite::Result<DbResult<SavedHostProfile>> {
    let metadata = metadata_row(row)?;
    let kind: String = row.get("kind")?;
    let config: String = row.get("native_config_json")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    Ok((|| {
        let kind = SavedHostKind::parse(&kind)
            .ok_or_else(|| DbError::Invalid(format!("unknown saved host kind {kind}")))?;
        let mut profile = Map::new();
        profile.insert("kind".into(), Value::from(kind.as_str()));
        if let Value::Object(config) = serde_json::from_str::<Value>(&config)? {
            profile.extend(config);
        }
        profile.insert("profileId".into(), Value::from(metadata.id.clone()));
        let profile: SavedHostSessionProfile = serde_json::from_value(Value::Object(profile))?;
        Ok(SavedHostProfile {
            id: metadata.id.clone(),
            kind,
            name: metadata.name.clone(),
            profile,
            metadata: metadata.metadata(false),
            created_at,
            updated_at,
        })
    })())
}

fn management_request_from_row(
    row: &Row<'_>,
) -> rusqlite::Result<DbResult<SavedManagementRequest>> {
    let id: String = row.get("id")?;
    let protocol: String = row.get("protocol")?;
    let profile_id: Option<String> = row.get("profile_id")?;
    let name: String = row.get("name")?;
    let request: String = row.get("request_json")?;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    Ok((|| {
        let protocol = ManagementProtocol::parse(&protocol)
            .ok_or_else(|| DbError::Invalid(format!("unknown management protocol {protocol}")))?;
        let request = match serde_json::from_str::<Value>(&request)? {
            Value::Object(map) => map,
            _ => Map::new(),
        };
        Ok(SavedManagementRequest {
            id,
            protocol,
            profile_id: optional_string(profile_id),
            name,
            request,
            created_at,
            updated_at,
        })
    })())
}

impl Database {
    /// Muxus metadata for the given OpenSSH aliases, in the order asked;
    /// aliases the database knows nothing about are left out.
    pub fn open_ssh_metadata<S: AsRef<str>>(
        &self,
        aliases: &[S],
    ) -> DbResult<Vec<(String, OpenSshProfileMetadata)>> {
        let conn = self.lock();
        let mut result = Vec::new();
        for alias in aliases {
            let alias = alias.as_ref();
            if let Some(row) = metadata_by_alias(&conn, alias)? {
                result.push((alias.to_owned(), row.metadata(true)));
            }
        }
        Ok(result)
    }

    /// The sidebar folder an OpenSSH alias lives in, for dial-time folder defaults.
    pub fn group_for_alias(&self, alias: &str) -> DbResult<Option<String>> {
        Ok(metadata_by_alias(&self.lock(), alias)?.and_then(|row| optional_string(row.group_name)))
    }

    /// The sidebar folder a Muxus-owned SSH host lives in.
    pub fn group_for_saved_host(&self, id: &str) -> DbResult<Option<String>> {
        let group: Option<Option<String>> = self
            .lock()
            .query_row(
                "
        SELECT groups.name AS group_name
        FROM connection_profiles AS profiles
        LEFT JOIN connection_groups AS groups ON groups.id = profiles.group_id
        WHERE profiles.id = ? AND profiles.kind = 'ssh'
      ",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(optional_string(group.flatten()))
    }

    /// Whether this OpenSSH alias must avoid SFTP and Unix shell probing.
    pub fn sftp_disabled_for_alias(&self, alias: &str) -> DbResult<bool> {
        Ok(metadata_by_alias(&self.lock(), alias)?.is_some_and(|row| row.disable_sftp))
    }

    /// Whether this OpenSSH alias needs the stricter console session shape.
    pub fn console_compatibility_for_alias(&self, alias: &str) -> DbResult<bool> {
        Ok(metadata_by_alias(&self.lock(), alias)?.is_some_and(|row| row.console_compatibility))
    }

    /// Whether one Muxus-owned SSH host must avoid SFTP and shell probing.
    pub fn sftp_disabled_for_saved_host(&self, id: &str) -> DbResult<bool> {
        self.saved_ssh_flag(id, "disable_sftp")
    }

    /// Whether one Muxus-owned SSH host needs the stricter console session shape.
    pub fn console_compatibility_for_saved_host(&self, id: &str) -> DbResult<bool> {
        self.saved_ssh_flag(id, "console_compatibility")
    }

    fn saved_ssh_flag(&self, id: &str, column: &str) -> DbResult<bool> {
        let value: Option<i64> = self
            .lock()
            .query_row(
                &format!("SELECT {column} FROM connection_profiles WHERE id = ? AND kind = 'ssh'"),
                [id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(value == Some(1))
    }

    /// Update an OpenSSH alias's Muxus metadata, creating its row on first use.
    pub fn update_open_ssh_metadata(
        &self,
        alias: &str,
        patch: &OpenSshMetadataPatch,
    ) -> DbResult<OpenSshProfileMetadata> {
        require_non_empty(alias, "alias")?;
        let conn = self.lock();
        let current = ensure_open_ssh_profile(&conn, alias)?;
        let display_name = match &patch.display_name {
            Patch::Keep => current.name.clone(),
            Patch::Clear => alias.to_owned(),
            Patch::Set(name) => {
                let trimmed = muxus_core::js::js_trim(name);
                if trimmed.is_empty() {
                    alias.to_owned()
                } else {
                    trimmed.to_owned()
                }
            }
        };
        apply_metadata_patch(&conn, &current, &display_name, patch)?;
        let row = metadata_by_alias(&conn, alias)?
            .ok_or_else(|| DbError::NotFound("OpenSSH profile not found".into()))?;
        Ok(row.metadata(true))
    }

    /// Persist one complete visual group order across both host sources.
    /// Rows for OpenSSH hosts are created lazily so even otherwise-unmodified
    /// hosts can participate in sorting; Muxus-owned hosts must already exist.
    pub fn reorder_managed_hosts(&self, refs: &[ManagedHostRef]) -> DbResult<()> {
        let keys: HashSet<String> = refs
            .iter()
            .map(|host| match host {
                ManagedHostRef::Ssh { alias } => format!("ssh:{alias}"),
                ManagedHostRef::Profile { id } => format!("profile:{id}"),
            })
            .collect();
        if keys.len() != refs.len() {
            return Err(DbError::Invalid("host order contains duplicates".into()));
        }
        for host in refs {
            match host {
                ManagedHostRef::Ssh { alias } => require_non_empty(alias, "alias")?,
                ManagedHostRef::Profile { id } => require_non_empty(id, "id")?,
            }
        }
        self.immediate(|conn| {
            for (index, host) in refs.iter().enumerate() {
                let id = match host {
                    ManagedHostRef::Profile { id } => {
                        let exists = conn
                            .query_row(
                                &format!("SELECT id FROM connection_profiles WHERE id = ? AND kind IN {SAVED_HOST_KINDS_SQL}"),
                                [id],
                                |_| Ok(()),
                            )
                            .optional()?;
                        if exists.is_none() {
                            return Err(DbError::NotFound("saved host not found".into()));
                        }
                        id.clone()
                    }
                    ManagedHostRef::Ssh { alias } => ensure_open_ssh_profile(conn, alias)?.id,
                };
                conn.execute(
                    "
        UPDATE connection_profiles
        SET sort_order = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
                    params![index as i64, id],
                )?;
            }
            Ok(())
        })
    }

    /// Count one more connection to an OpenSSH alias.
    pub fn record_open_ssh_connection(&self, alias: &str) -> DbResult<OpenSshProfileMetadata> {
        require_non_empty(alias, "alias")?;
        let conn = self.lock();
        let current = ensure_open_ssh_profile(&conn, alias)?;
        conn.execute(
            "
        UPDATE connection_profiles
        SET last_connected_at = CURRENT_TIMESTAMP,
            connect_count = connect_count + 1,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
            [&current.id],
        )?;
        let row = metadata_by_alias(&conn, alias)?
            .ok_or_else(|| DbError::NotFound("OpenSSH profile not found".into()))?;
        Ok(row.metadata(true))
    }

    /// Preserve the stable profile ID when a Host alias is renamed externally.
    pub fn rename_open_ssh_alias(&self, previous_alias: &str, next_alias: &str) -> DbResult<()> {
        if previous_alias == next_alias {
            return Ok(());
        }
        require_non_empty(previous_alias, "previousAlias")?;
        require_non_empty(next_alias, "nextAlias")?;
        let (previous, next) = {
            let conn = self.lock();
            (
                metadata_by_alias(&conn, previous_alias)?,
                metadata_by_alias(&conn, next_alias)?,
            )
        };
        self.immediate(|conn| {
            Database::move_session_logging_policy(
                conn,
                &format!("ssh:{previous_alias}"),
                &format!("ssh:{next_alias}"),
            )?;
            let Some(previous) = previous else {
                return Ok(());
            };
            if let Some(next) = next {
                conn.execute(
                    "
            UPDATE connection_profiles
            SET connect_count = connect_count + ?,
                last_connected_at = CASE
                  WHEN last_connected_at IS NULL OR ? > last_connected_at THEN ?
                  ELSE last_connected_at
                END,
                updated_at = CURRENT_TIMESTAMP
            WHERE id = ?
          ",
                    params![
                        next.connect_count,
                        optional_string(next.last_connected_at.clone()).unwrap_or_default(),
                        next.last_connected_at,
                        previous.id,
                    ],
                )?;
                conn.execute("DELETE FROM connection_profiles WHERE id = ?", [&next.id])?;
            }
            let old_name_was_alias = previous.name == previous_alias;
            conn.execute(
                "
          UPDATE connection_profiles
          SET ssh_alias = ?, name = ?, updated_at = CURRENT_TIMESTAMP
          WHERE id = ?
        ",
                params![
                    next_alias,
                    if old_name_was_alias {
                        next_alias
                    } else {
                        previous.name.as_str()
                    },
                    previous.id
                ],
            )?;
            Ok(())
        })
    }

    /// Store a raw native connection row; returns its new id.
    pub fn create_native_connection(&self, input: &NativeConnectionInput) -> DbResult<String> {
        require_non_empty(&input.name, "name")?;
        assert_secret_free(&input.config, "config")?;
        let id = nanoid();
        self.lock().execute(
            "
        INSERT INTO connection_profiles(
          id, kind, name, native_config_json, credential_ref_id
        ) VALUES (?, ?, ?, ?, ?)
      ",
            params![
                id,
                input.kind.as_str(),
                muxus_core::js::js_trim(&input.name),
                serde_json::to_string(&input.config)?,
                input.credential_ref_id,
            ],
        )?;
        Ok(id)
    }

    /// Every Muxus-owned host, in sidebar order then by name. A row whose
    /// stored profile this build cannot read is left out.
    pub fn list_saved_host_profiles(&self) -> DbResult<Vec<SavedHostProfile>> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "{} ORDER BY profiles.sort_order, profiles.name COLLATE NOCASE",
            saved_host_sql("1 = 1")
        ))?;
        let rows = statement.query_map([], saved_host_from_row)?;
        let mut profiles = Vec::new();
        for row in rows {
            if let Ok(profile) = row? {
                profiles.push(profile);
            }
        }
        Ok(profiles)
    }

    /// Create or replace a Muxus-owned host. A supplied id makes the save an
    /// idempotent upsert (import, restore).
    pub fn save_saved_host_profile(
        &self,
        input: &SavedHostProfileInput,
    ) -> DbResult<SavedHostProfile> {
        require_non_empty(&input.name, "name")?;
        if let SavedHostSessionProfile::Ssh(ssh) = &input.profile
            && ssh.use_config != Some(false)
        {
            return Err(DbError::Invalid(
                "Muxus-owned SSH profiles must set useConfig to false".into(),
            ));
        }
        let kind = input.profile.kind();
        let config = Value::Object(saved_host_config(&input.profile)?);
        assert_secret_free(&config, "profile.config")?;
        let id = input.id.clone().unwrap_or_else(nanoid);
        let name = muxus_core::js::js_trim(&input.name);
        {
            let conn = self.lock();
            let current: Option<String> = conn
                .query_row(
                    "SELECT kind FROM connection_profiles WHERE id = ?",
                    [&id],
                    |row| row.get(0),
                )
                .optional()?;
            if let Some(current) = current {
                if SavedHostKind::parse(&current).is_none() {
                    return Err(DbError::Invalid(
                        "profile ID belongs to a different connection type".into(),
                    ));
                }
                conn.execute(
                    "
          UPDATE connection_profiles
          SET kind = ?, name = ?, native_config_json = ?, updated_at = CURRENT_TIMESTAMP
          WHERE id = ?
        ",
                    params![kind.as_str(), name, serde_json::to_string(&config)?, id],
                )?;
            } else {
                conn.execute(
                    "
          INSERT INTO connection_profiles(id, kind, name, native_config_json)
          VALUES (?, ?, ?, ?)
        ",
                    params![id, kind.as_str(), name, serde_json::to_string(&config)?],
                )?;
            }
        }
        self.saved_host_profile(&id)?
            .ok_or_else(|| DbError::NotFound("saved host not found".into()))
    }

    /// One Muxus-owned host.
    pub fn saved_host_profile(&self, id: &str) -> DbResult<Option<SavedHostProfile>> {
        let conn = self.lock();
        let row = conn
            .query_row(
                &saved_host_sql("profiles.id = ?"),
                [id],
                saved_host_from_row,
            )
            .optional()?;
        row.transpose()
    }

    /// Update a saved host's Muxus metadata.
    pub fn update_saved_host_metadata(
        &self,
        id: &str,
        patch: &OpenSshMetadataPatch,
    ) -> DbResult<SavedHostProfile> {
        {
            let conn = self.lock();
            let current = conn
                .query_row(&saved_host_sql("profiles.id = ?"), [id], metadata_row)
                .optional()?
                .ok_or_else(|| DbError::NotFound("saved host not found".into()))?;
            let name = match &patch.display_name {
                Patch::Keep | Patch::Clear => current.name.clone(),
                Patch::Set(name) => {
                    let trimmed = muxus_core::js::js_trim(name);
                    if trimmed.is_empty() {
                        current.name.clone()
                    } else {
                        trimmed.to_owned()
                    }
                }
            };
            apply_metadata_patch(&conn, &current, &name, patch)?;
        }
        self.saved_host_profile(id)?
            .ok_or_else(|| DbError::NotFound("saved host not found".into()))
    }

    /// Count one more connection to a saved host.
    pub fn record_saved_host_connection(&self, id: &str) -> DbResult<()> {
        self.lock().execute(
            &format!(
                "
        UPDATE connection_profiles
        SET last_connected_at = CURRENT_TIMESTAMP,
            connect_count = connect_count + 1,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = ? AND kind IN {SAVED_HOST_KINDS_SQL}
      "
            ),
            [id],
        )?;
        Ok(())
    }

    /// Delete a saved host with its logging policy and its own saved requests.
    pub fn delete_saved_host_profile(&self, id: &str) -> DbResult<bool> {
        let conn = self.lock();
        let deleted = conn.execute(
            &format!(
                "DELETE FROM connection_profiles WHERE id = ? AND kind IN {SAVED_HOST_KINDS_SQL}"
            ),
            [id],
        )? > 0;
        if deleted {
            conn.execute(
                "DELETE FROM session_logging_policies WHERE profile_key = ?",
                [format!("profile:{id}")],
            )?;
            conn.execute("DELETE FROM management_requests WHERE profile_id = ?", [id])?;
        }
        Ok(deleted)
    }

    /// A host's own login sequence and the folder it lives in. `None` when the
    /// database knows nothing about the host, which then has neither.
    pub fn host_login_sequence(&self, host: HostRef<'_>) -> DbResult<Option<HostLoginSequence>> {
        let conn = self.lock();
        let row = match host {
            HostRef::Alias(alias) => metadata_by_alias(&conn, alias)?,
            HostRef::ProfileId(id) => conn
                .query_row(&saved_host_sql("profiles.id = ?"), [id], metadata_row)
                .optional()?,
        };
        Ok(row.map(|row| HostLoginSequence {
            login_sequence: login_sequence_from_json(row.login_sequence_json.as_deref()),
            group: optional_string(row.group_name),
        }))
    }

    /// Saved requests offered on a host: its own and the protocol-wide ones
    /// (only the protocol-wide ones without a profile id).
    pub fn list_management_requests(
        &self,
        protocol: ManagementProtocol,
        profile_id: Option<&str>,
    ) -> DbResult<Vec<SavedManagementRequest>> {
        let conn = self.lock();
        let mut statement = conn.prepare(
            "
        SELECT id, protocol, profile_id, name, request_json, created_at, updated_at
        FROM management_requests
        WHERE protocol = ? AND (profile_id IS NULL OR profile_id = ?)
        ORDER BY name COLLATE NOCASE, created_at
      ",
        )?;
        let rows = statement.query_map(
            params![protocol.as_str(), profile_id],
            management_request_from_row,
        )?;
        let mut requests = Vec::new();
        for row in rows {
            requests.push(row??);
        }
        Ok(requests)
    }

    /// Create or replace a saved request.
    pub fn save_management_request(
        &self,
        input: &SavedManagementRequestInput,
    ) -> DbResult<SavedManagementRequest> {
        require_non_empty(&input.name, "name")?;
        let request = Value::Object(input.request.clone());
        assert_secret_free(&request, "request")?;
        let id = input.id.clone().unwrap_or_else(nanoid);
        let conn = self.lock();
        conn.execute(
            "
        INSERT INTO management_requests(id, protocol, profile_id, name, request_json)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          protocol = excluded.protocol,
          profile_id = excluded.profile_id,
          name = excluded.name,
          request_json = excluded.request_json,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![
                id,
                input.protocol.as_str(),
                input.profile_id,
                muxus_core::js::js_trim(&input.name),
                serde_json::to_string(&request)?
            ],
        )?;
        conn.query_row(
            "
        SELECT id, protocol, profile_id, name, request_json, created_at, updated_at
        FROM management_requests WHERE id = ?
      ",
            [&id],
            management_request_from_row,
        )?
    }

    pub fn delete_management_request(&self, id: &str) -> DbResult<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM management_requests WHERE id = ?", [id])?
            > 0)
    }

    /// The RDP certificate or VNC server key trusted for a host, keyed by the
    /// SSH gateway it is reached through (`""` for a direct connection).
    pub fn trusted_desktop_identity(
        &self,
        host: &str,
        port: u16,
        gateway: &str,
    ) -> DbResult<Option<DesktopIdentity>> {
        Ok(self
            .lock()
            .query_row(
                "
        SELECT fingerprint, subject FROM remote_desktop_certificates
        WHERE host = ? AND port = ? AND gateway = ?
      ",
                params![host.to_lowercase(), port, gateway],
                |row| {
                    Ok(DesktopIdentity {
                        fingerprint: row.get(0)?,
                        subject: row.get(1)?,
                    })
                },
            )
            .optional()?)
    }

    /// Pin (or replace) the identity trusted for a host.
    pub fn trust_desktop_identity(
        &self,
        host: &str,
        port: u16,
        gateway: &str,
        fingerprint: &str,
        subject: &str,
    ) -> DbResult<()> {
        require_non_empty(host, "host")?;
        require_non_empty(fingerprint, "fingerprint")?;
        self.lock().execute(
            "
        INSERT INTO remote_desktop_certificates(host, port, gateway, fingerprint, subject)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(host, port, gateway) DO UPDATE SET
          fingerprint = excluded.fingerprint,
          subject = excluded.subject,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![host.to_lowercase(), port, gateway, fingerprint, subject],
        )?;
        Ok(())
    }
}

impl Database {
    /// The sequence a session to this host runs (`resolveLoginSequence`): the
    /// host's own, or else the nearest folder's that sets one. An empty
    /// sequence on the host or on a folder means "none" and ends the search,
    /// so a folder's sequence can be switched off for one host or subfolder.
    pub fn resolve_login_sequence(&self, host: HostRef<'_>) -> DbResult<Option<LoginSequence>> {
        let Some(entry) = self.host_login_sequence(host)? else {
            return Ok(None);
        };
        let mut sequence = entry.login_sequence;
        if sequence.is_none() {
            for path in
                muxus_core::folder_paths::folder_chain(entry.group.as_deref().unwrap_or_default())
            {
                sequence = self
                    .folder_settings_for_path(&path)?
                    .and_then(|row| row.login_sequence);
                if sequence.is_some() {
                    break;
                }
            }
        }
        Ok(sequence.filter(|sequence| !sequence.steps.is_empty()))
    }
}
