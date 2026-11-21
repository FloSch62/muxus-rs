//! Shared session lists (`shared_session_sources` in database.ts): the links Muxus follows
//! and the saved hosts each one keeps up to date. A check only touches a host whose entry
//! changed in the list, and then only the settings the list changed, so what the user set
//! on the host stays.

use std::collections::{BTreeMap, HashSet};

use chrono::{DateTime, Utc};
use muxus_core::api::{
    OpenSshMetadataPatch, SavedHostProfileInput, SharedSessionHost, SharedSessionSource,
    SharedSessionSourceInput,
};
use muxus_core::folder_paths::{
    folder_path_key, folder_path_segments, is_descendant_folder_path, rename_folder_path_under,
};
use muxus_core::js::{Patch, iso_timestamp, js_trim, utf16_prefix};
use muxus_core::protocol::SavedHostSessionProfile;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Map, Value};

use super::hosts::{
    SAVED_HOST_KINDS_SQL, delete_saved_host_on, group_id_for_name, save_saved_host_on,
    saved_host_on, update_saved_host_metadata_on,
};
use super::{Database, DbError, DbResult, optional_string, require_non_empty};
use crate::ids::nanoid;

const SOURCE_SELECT: &str = "
  SELECT sources.*, (
    SELECT COUNT(*) FROM connection_profiles WHERE shared_source_id = sources.id
  ) AS host_count
  FROM shared_session_sources AS sources
";

fn source_from_row(row: &Row<'_>) -> rusqlite::Result<SharedSessionSource> {
    Ok(SharedSessionSource {
        id: row.get("id")?,
        name: row.get("name")?,
        url: row.get("url")?,
        folder: row.get("folder")?,
        refresh_minutes: row
            .get::<_, i64>("refresh_minutes")?
            .clamp(0, i64::from(u32::MAX)) as u32,
        host_count: row.get::<_, i64>("host_count")?.max(0) as u64,
        last_checked_at: optional_string(row.get("last_checked_at")?),
        last_synced_at: optional_string(row.get("last_synced_at")?),
        last_error: optional_string(row.get("last_error")?),
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

fn source_on(conn: &Connection, id: &str) -> DbResult<Option<SharedSessionSource>> {
    Ok(conn
        .query_row(
            &format!("{SOURCE_SELECT} WHERE sources.id = ?"),
            [id],
            source_from_row,
        )
        .optional()?)
}

/// What claiming one read of a link gave.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SharedSessionCheck {
    /// Read it: the link as it was before this check.
    Read(Box<SharedSessionSource>),
    /// Another window read it within its interval.
    NotDue,
}

/// What applying a list changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SharedSessionCounts {
    pub added: u64,
    pub updated: u64,
    pub removed: u64,
    pub unchanged: u64,
}

/// What a list says about one host, kept to tell what it changes next time.
struct Entry {
    name: String,
    profile: Map<String, Value>,
    /// Where the host sits inside the link's own folder. That folder is left out, so
    /// renaming or moving it does not make every entry look changed.
    folder: String,
    color: Option<String>,
    icon: Option<String>,
}

impl Entry {
    fn of(host: &SharedSessionHost, link_folder: &str) -> DbResult<Self> {
        let group = host.group.as_deref().unwrap_or("");
        let folder = if folder_path_key(group) == folder_path_key(link_folder) {
            String::new()
        } else if is_descendant_folder_path(group, link_folder) {
            folder_path_segments(group)[folder_path_segments(link_folder).len()..].join("/")
        } else {
            format!("/{group}")
        };
        let Value::Object(profile) = serde_json::to_value(&host.profile)? else {
            return Err(DbError::Invalid("profile must be an object".into()));
        };
        Ok(Self {
            name: host.name.clone(),
            profile,
            folder,
            color: host.color.clone(),
            icon: host.icon.clone(),
        })
    }

    fn parse(json: &str) -> Option<Self> {
        let value: Value = serde_json::from_str(json).ok()?;
        let text = |key: &str| value.get(key).and_then(Value::as_str).map(str::to_owned);
        Some(Self {
            name: text("name")?,
            profile: value.get("profile")?.as_object()?.clone(),
            folder: text("folder")?,
            color: text("color"),
            icon: text("icon"),
        })
    }

    fn json(&self) -> String {
        let mut entry = Map::new();
        entry.insert("name".into(), Value::from(self.name.clone()));
        entry.insert("profile".into(), Value::Object(self.profile.clone()));
        entry.insert("folder".into(), Value::from(self.folder.clone()));
        if let Some(color) = &self.color {
            entry.insert("color".into(), Value::from(color.clone()));
        }
        if let Some(icon) = &self.icon {
            entry.insert("icon".into(), Value::from(icon.clone()));
        }
        canonical_json(Some(&Value::Object(entry)))
    }
}

/// JSON with every object's keys in order, so two values that say the same compare equal as
/// text; a value that is not there reads as `null` (`canonicalJson`).
fn canonical_json(value: Option<&Value>) -> String {
    match value {
        None => "null".into(),
        Some(Value::Array(items)) => format!(
            "[{}]",
            items
                .iter()
                .map(|item| canonical_json(Some(item)))
                .collect::<Vec<_>>()
                .join(",")
        ),
        Some(Value::Object(map)) => {
            // In the order JavaScript's `<` puts the keys: by UTF-16 code unit.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_by(|(a, _), (b, _)| a.encode_utf16().cmp(b.encode_utf16()));
            format!(
                "{{{}}}",
                entries
                    .into_iter()
                    .map(|(key, entry)| format!(
                        "{}:{}",
                        Value::from(key.as_str()),
                        canonical_json(Some(entry))
                    ))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        }
        Some(other) => other.to_string(),
    }
}

/// A host's connection settings after its list entry changed: as they are here, with every
/// setting the list changed taken from the list. A list that turns the host into another
/// kind of connection replaces them all.
fn merged_profile(
    current: &SavedHostSessionProfile,
    previous: &Map<String, Value>,
    next: &Map<String, Value>,
) -> DbResult<SavedHostSessionProfile> {
    let Value::Object(mut merged) = serde_json::to_value(current)? else {
        return Err(DbError::Invalid("profile must be an object".into()));
    };
    let same_kind =
        previous.get("kind") == next.get("kind") && merged.get("kind") == next.get("kind");
    if !same_kind {
        return Ok(serde_json::from_value(Value::Object(next.clone()))?);
    }
    let keys: HashSet<&String> = previous.keys().chain(next.keys()).collect();
    for key in keys {
        if canonical_json(previous.get(key)) == canonical_json(next.get(key)) {
            continue;
        }
        match next.get(key) {
            Some(value) => merged.insert(key.clone(), value.clone()),
            None => merged.remove(key),
        };
    }
    Ok(serde_json::from_value(Value::Object(merged))?)
}

/// The hosts a link keeps, with what its list last said about each.
fn entries_of(conn: &Connection, source_id: &str) -> DbResult<BTreeMap<String, String>> {
    let mut statement = conn.prepare(&format!(
        "SELECT id, shared_entry_json FROM connection_profiles
         WHERE shared_source_id = ? AND kind IN {SAVED_HOST_KINDS_SQL}"
    ))?;
    let rows = statement.query_map([source_id], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, Option<String>>(1)?.unwrap_or_default(),
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn option_patch(value: &Option<String>) -> Patch<String> {
    match value {
        Some(value) => Patch::Set(value.clone()),
        None => Patch::Clear,
    }
}

impl Database {
    /// Every link, by name.
    pub fn list_shared_session_sources(&self) -> DbResult<Vec<SharedSessionSource>> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "{SOURCE_SELECT} ORDER BY sources.name COLLATE NOCASE, sources.created_at"
        ))?;
        let rows = statement.query_map([], source_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    pub fn shared_session_source(&self, id: &str) -> DbResult<Option<SharedSessionSource>> {
        source_on(&self.lock(), id)
    }

    /// Follow a link, or change one. `now` is when a link added by hand counts as read.
    pub fn save_shared_session_source(
        &self,
        input: &SharedSessionSourceInput,
        now: DateTime<Utc>,
    ) -> DbResult<SharedSessionSource> {
        require_non_empty(&input.name, "name")?;
        require_non_empty(&input.url, "url")?;
        require_non_empty(&input.folder, "folder")?;
        let id = input.id.clone().unwrap_or_else(nanoid);
        let conn = self.lock();
        let previous = source_on(&conn, &id)?;
        conn.execute(
            "
        INSERT INTO shared_session_sources(id, name, url, folder, refresh_minutes)
        VALUES (?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          name = excluded.name,
          url = excluded.url,
          folder = excluded.folder,
          refresh_minutes = excluded.refresh_minutes,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![
                id,
                js_trim(&input.name),
                input.url,
                input.folder,
                i64::from(input.refresh_minutes)
            ],
        )?;
        match &previous {
            // Whoever adds a link by hand reads it at once. Marking it checked keeps other
            // windows from reading it a second time in the same moment. A link restored
            // with its ID has no one reading it, so it stays due.
            None if input.id.is_none() => {
                conn.execute(
                    "UPDATE shared_session_sources SET last_checked_at = ? WHERE id = ?",
                    params![iso_timestamp(now), id],
                )?;
            }
            None => {}
            Some(previous) => {
                // Another link is read at the next opportunity, not an interval later.
                if previous.url != input.url {
                    conn.execute(
                        "UPDATE shared_session_sources SET last_checked_at = NULL, last_error = NULL WHERE id = ?",
                        [&id],
                    )?;
                }
                // Another folder takes the hosts along. One the user moved out of it stays
                // where it is.
                if previous.folder != input.folder {
                    let hosts: Vec<(String, Option<String>)> = {
                        let mut statement = conn.prepare(
                            "
          SELECT profiles.id, groups.name AS group_name
          FROM connection_profiles AS profiles
          LEFT JOIN connection_groups AS groups ON groups.id = profiles.group_id
          WHERE profiles.shared_source_id = ?
        ",
                        )?;
                        let rows =
                            statement.query_map([&id], |row| Ok((row.get(0)?, row.get(1)?)))?;
                        rows.collect::<Result<_, _>>()?
                    };
                    for (host, group) in hosts {
                        let Some(next) = rename_folder_path_under(
                            group.as_deref().unwrap_or(""),
                            &previous.folder,
                            &input.folder,
                        ) else {
                            continue;
                        };
                        conn.execute(
                            "UPDATE connection_profiles SET group_id = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                            params![group_id_for_name(&conn, Some(&next))?, host],
                        )?;
                    }
                }
            }
        }
        source_on(&conn, &id)?
            .ok_or_else(|| DbError::NotFound("shared sessions link not found".into()))
    }

    /// A link's folder follows when it, or a folder around it, is renamed or moved in the
    /// sidebar. The hosts are moved by whoever renames the folder.
    pub fn move_shared_session_folders(&self, from: &str, to: &str) -> DbResult<()> {
        let sources = self.list_shared_session_sources()?;
        let conn = self.lock();
        for source in sources {
            let Some(next) = rename_folder_path_under(&source.folder, from, to) else {
                continue;
            };
            if next == source.folder {
                continue;
            }
            conn.execute(
                "UPDATE shared_session_sources SET folder = ?, updated_at = CURRENT_TIMESTAMP WHERE id = ?",
                params![next, source.id],
            )?;
        }
        Ok(())
    }

    /// Remove a link, and its hosts with it unless they are kept as hosts of the user's own.
    pub fn delete_shared_session_source(&self, id: &str, keep_hosts: bool) -> DbResult<bool> {
        self.immediate(|conn| {
            if keep_hosts {
                conn.execute(
                    "
            UPDATE connection_profiles
            SET shared_source_id = NULL, shared_entry_json = NULL
            WHERE shared_source_id = ?
          ",
                    [id],
                )?;
            } else {
                for host in entries_of(conn, id)?.keys() {
                    delete_saved_host_on(conn, host)?;
                }
            }
            Ok(conn.execute("DELETE FROM shared_session_sources WHERE id = ?", [id])? > 0)
        })
    }

    /// Claim one read of a link; `None` when there is no such link. Unless forced, a link
    /// another window read within its interval is left alone, so several windows do not
    /// each fetch it.
    pub fn begin_shared_session_check(
        &self,
        id: &str,
        force: bool,
        now: DateTime<Utc>,
    ) -> DbResult<Option<SharedSessionCheck>> {
        let conn = self.lock();
        let Some(source) = source_on(&conn, id)? else {
            return Ok(None);
        };
        if !force && !source.due(now.timestamp_millis()) {
            return Ok(Some(SharedSessionCheck::NotDue));
        }
        conn.execute(
            "UPDATE shared_session_sources SET last_checked_at = ? WHERE id = ?",
            params![iso_timestamp(now), id],
        )?;
        Ok(Some(SharedSessionCheck::Read(Box::new(source))))
    }

    /// A link that could not be read or understood keeps the hosts it had.
    pub fn fail_shared_session_check(
        &self,
        id: &str,
        error: &str,
    ) -> DbResult<Option<SharedSessionSource>> {
        let said = utf16_prefix(js_trim(error), 1000);
        let conn = self.lock();
        conn.execute(
            "UPDATE shared_session_sources SET last_error = ? WHERE id = ?",
            params![
                if said.is_empty() {
                    "The shared sessions could not be read."
                } else {
                    said
                },
                id
            ],
        )?;
        source_on(&conn, id)
    }

    /// Make a link's hosts follow its list; `None` when there is no such link. New entries
    /// are added and hosts the list no longer has are deleted. An entry that did not change
    /// is not touched. One that did change brings over only the settings the list changed,
    /// so a user name, key or anything else set on the host here stays unless the list
    /// changes that same setting.
    pub fn apply_shared_sessions(
        &self,
        source_id: &str,
        hosts: &[SharedSessionHost],
        now: DateTime<Utc>,
    ) -> DbResult<Option<SharedSessionCounts>> {
        let Some(source) = self.shared_session_source(source_id)? else {
            return Ok(None);
        };
        self.immediate(|conn| {
            let mut counts = SharedSessionCounts::default();
            let mut stale = entries_of(conn, source_id)?;
            let mut seen = HashSet::new();
            for host in hosts {
                if !seen.insert(host.id.as_str()) {
                    return Err(DbError::Invalid(format!(
                        "The shared sessions list “{}” twice.",
                        host.name
                    )));
                }
                let entry = Entry::of(host, &source.folder)?;
                let entry_json = entry.json();
                let owner: Option<Option<String>> = conn
                    .query_row(
                        "SELECT shared_source_id FROM connection_profiles WHERE id = ?",
                        [&host.id],
                        |row| row.get(0),
                    )
                    .optional()?;
                if let Some(Some(other)) = &owner
                    && other != source_id
                {
                    return Err(DbError::Invalid(format!(
                        "“{}” already belongs to another shared sessions link.",
                        host.name
                    )));
                }
                let previous_json = stale.remove(&host.id);
                if previous_json.as_deref() == Some(entry_json.as_str()) {
                    counts.unchanged += 1;
                    continue;
                }
                let current = match &previous_json {
                    Some(_) => saved_host_on(conn, &host.id)?,
                    None => None,
                };
                let previous = previous_json.as_deref().and_then(Entry::parse);
                match (current, previous) {
                    (Some(current), Some(previous)) => {
                        save_saved_host_on(
                            conn,
                            &SavedHostProfileInput {
                                id: Some(host.id.clone()),
                                name: if previous.name == entry.name {
                                    current.name.clone()
                                } else {
                                    entry.name.clone()
                                },
                                profile: merged_profile(
                                    &current.profile,
                                    &previous.profile,
                                    &entry.profile,
                                )?,
                            },
                        )?;
                        let mut patch = OpenSshMetadataPatch::default();
                        if previous.folder != entry.folder {
                            patch.group = option_patch(&host.group);
                        }
                        if previous.color != entry.color {
                            patch.color = option_patch(&entry.color);
                        }
                        if previous.icon != entry.icon {
                            patch.icon = option_patch(&entry.icon);
                        }
                        update_saved_host_metadata_on(conn, &host.id, &patch)?;
                    }
                    _ => {
                        // New, or a host that was there without this link (restored from
                        // a backup): nothing says what the user changed on it, so the list
                        // decides.
                        save_saved_host_on(
                            conn,
                            &SavedHostProfileInput {
                                id: Some(host.id.clone()),
                                name: host.name.clone(),
                                profile: host.profile.clone(),
                            },
                        )?;
                        let mut patch = OpenSshMetadataPatch {
                            group: option_patch(&host.group),
                            ..Default::default()
                        };
                        if let Some(color) = &host.color {
                            patch.color = Patch::Set(color.clone());
                        }
                        if let Some(icon) = &host.icon {
                            patch.icon = Patch::Set(icon.clone());
                        }
                        update_saved_host_metadata_on(conn, &host.id, &patch)?;
                    }
                }
                conn.execute(
                    "UPDATE connection_profiles SET shared_source_id = ?, shared_entry_json = ? WHERE id = ?",
                    params![source_id, entry_json, host.id],
                )?;
                if owner.is_some() {
                    counts.updated += 1;
                } else {
                    counts.added += 1;
                }
            }
            for host in stale.keys() {
                delete_saved_host_on(conn, host)?;
                counts.removed += 1;
            }
            let at = iso_timestamp(now);
            conn.execute(
                "
            UPDATE shared_session_sources
            SET last_synced_at = ?, last_checked_at = ?, last_error = NULL
            WHERE id = ?
          ",
                params![at, at, source_id],
            )?;
            Ok(Some(counts))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn writes_objects_with_their_keys_in_order() {
        let value = json!({ "b": [2, { "z": null, "a": "x" }], "a": 1.5, "c": true });
        assert_eq!(
            canonical_json(Some(&value)),
            r#"{"a":1.5,"b":[2,{"a":"x","z":null}],"c":true}"#
        );
        // A setting that is not there says the same as one that is null.
        assert_eq!(canonical_json(None), canonical_json(Some(&Value::Null)));
        assert_eq!(canonical_json(Some(&json!("a\"b"))), r#""a\"b""#);
    }

    #[test]
    fn keeps_what_the_list_did_not_change() {
        let profile = |value: Value| -> Map<String, Value> { value.as_object().unwrap().clone() };
        let current: SavedHostSessionProfile = serde_json::from_value(json!({
            "kind": "ssh", "target": "web1.example.com", "useConfig": false,
            "user": "me", "port": 2222, "identityFiles": ["~/.ssh/mine"],
        }))
        .unwrap();
        let previous = profile(json!({
            "kind": "ssh", "target": "web1.example.com", "useConfig": false, "port": 22,
            "user": "ops",
        }));
        // The list moves the host and drops its port; the user is as it was in the list.
        let next = profile(json!({
            "kind": "ssh", "target": "web1.internal", "useConfig": false, "user": "ops",
        }));
        let merged =
            serde_json::to_value(merged_profile(&current, &previous, &next).unwrap()).unwrap();
        assert_eq!(merged["target"], "web1.internal");
        assert_eq!(merged["user"], "me", "the user's own user name stays");
        assert_eq!(merged["identityFiles"], json!(["~/.ssh/mine"]));
        assert_eq!(
            merged.get("port"),
            None,
            "the port the list dropped is gone"
        );

        // Another kind of connection replaces everything.
        let telnet = profile(json!({ "kind": "telnet", "host": "web1.internal", "port": 23 }));
        let replaced = merged_profile(&current, &previous, &telnet).unwrap();
        assert!(matches!(replaced, SavedHostSessionProfile::Telnet(_)));
    }
}
