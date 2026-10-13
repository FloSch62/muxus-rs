//! Workspaces (saved window layouts) and terminal scrollback snapshots.

use std::collections::HashSet;

use muxus_core::api::{
    TerminalSnapshotRecord, WorkspaceLayoutV1, WorkspaceMultiExecGroup, WorkspaceRecord,
    WorkspaceSummary,
};
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::Value;

use super::{Database, DbError, DbResult, assert_secret_free, optional_string, require_non_empty};
use crate::ids::nanoid;

/// A workspace to save. The layout is stored as given (its shape is not
/// checked here, as in the TypeScript database layer).
#[derive(Debug, Clone, PartialEq)]
pub struct WorkspaceInput {
    pub id: Option<String>,
    pub name: String,
    pub layout: Value,
    pub multi_exec_groups: Option<Vec<WorkspaceMultiExecGroup>>,
}

impl WorkspaceInput {
    /// A typed layout to save.
    pub fn new(
        id: Option<String>,
        name: impl Into<String>,
        layout: &WorkspaceLayoutV1,
        groups: Vec<WorkspaceMultiExecGroup>,
    ) -> DbResult<Self> {
        Ok(Self {
            id,
            name: name.into(),
            layout: serde_json::to_value(layout)?,
            multi_exec_groups: Some(groups),
        })
    }
}

/// A stored workspace with its layout as stored JSON (the TypeScript
/// `WorkspaceRecord` of the database layer, whose layout is `unknown`).
#[serde_with::skip_serializing_none]
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StoredWorkspace {
    pub id: String,
    pub name: String,
    pub layout: Value,
    pub multi_exec_groups: Vec<WorkspaceMultiExecGroup>,
    pub is_locked: bool,
    pub is_startup: bool,
    pub created_at: String,
    pub updated_at: String,
    pub last_opened_at: Option<String>,
}

impl StoredWorkspace {
    /// The workspace with its layout read as a version 1 layout.
    pub fn to_record(&self) -> DbResult<WorkspaceRecord> {
        Ok(WorkspaceRecord {
            id: self.id.clone(),
            name: self.name.clone(),
            layout: serde_json::from_value::<WorkspaceLayoutV1>(self.layout.clone())?,
            multi_exec_groups: self.multi_exec_groups.clone(),
            is_locked: self.is_locked,
            is_startup: self.is_startup,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            last_opened_at: self.last_opened_at.clone(),
        })
    }

    pub fn summary(&self) -> WorkspaceSummary {
        WorkspaceSummary {
            id: self.id.clone(),
            name: self.name.clone(),
            is_locked: self.is_locked,
            is_startup: self.is_startup,
            created_at: self.created_at.clone(),
            updated_at: self.updated_at.clone(),
            last_opened_at: self.last_opened_at.clone(),
        }
    }
}

const WORKSPACE_COLUMNS: &str = "
        SELECT id, name, layout_json, multi_exec_groups_json, is_locked, is_startup,
               created_at, updated_at, last_opened_at
        FROM workspaces";

const NEWEST_FIRST: &str =
    "ORDER BY COALESCE(last_opened_at, updated_at) DESC, name COLLATE NOCASE";

fn workspace_from_row(row: &Row<'_>) -> rusqlite::Result<DbResult<StoredWorkspace>> {
    let layout: String = row.get("layout_json")?;
    let groups: String = row.get("multi_exec_groups_json")?;
    let id: String = row.get("id")?;
    let name: String = row.get("name")?;
    let is_locked = row.get::<_, i64>("is_locked")? == 1;
    let is_startup = row.get::<_, i64>("is_startup")? == 1;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    let last_opened_at = optional_string(row.get("last_opened_at")?);
    Ok((|| {
        Ok(StoredWorkspace {
            id,
            name,
            layout: serde_json::from_str(&layout)?,
            multi_exec_groups: serde_json::from_str(&groups)?,
            is_locked,
            is_startup,
            created_at,
            updated_at,
            last_opened_at,
        })
    })())
}

fn workspace(conn: &Connection, id: &str) -> DbResult<Option<StoredWorkspace>> {
    conn.query_row(
        &format!("{WORKSPACE_COLUMNS} WHERE id = ?"),
        [id],
        workspace_from_row,
    )
    .optional()?
    .transpose()
}

fn list(conn: &Connection, sql: &str) -> DbResult<Vec<StoredWorkspace>> {
    let mut statement = conn.prepare(sql)?;
    let rows = statement.query_map([], workspace_from_row)?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row??);
    }
    Ok(out)
}

fn save_workspace(
    conn: &Connection,
    input: &WorkspaceInput,
    overwrite_locked: bool,
) -> DbResult<String> {
    require_non_empty(&input.name, "name")?;
    assert_secret_free(&input.layout, "workspace.layout")?;
    let groups = input.multi_exec_groups.clone().unwrap_or_default();
    assert_secret_free(&serde_json::to_value(&groups)?, "workspace.multiExecGroups")?;
    if let Some(id) = &input.id
        && !overwrite_locked
    {
        let locked: Option<i64> = conn
            .query_row(
                "SELECT is_locked FROM workspaces WHERE id = ?",
                [id],
                |row| row.get(0),
            )
            .optional()?;
        if locked == Some(1) {
            return Err(DbError::WorkspaceLocked(id.clone()));
        }
    }
    let id = input.id.clone().unwrap_or_else(nanoid);
    conn.execute(
        "
        INSERT INTO workspaces(id, name, layout_json, multi_exec_groups_json)
        VALUES (?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          name = excluded.name,
          layout_json = excluded.layout_json,
          multi_exec_groups_json = excluded.multi_exec_groups_json,
          updated_at = CURRENT_TIMESTAMP
      ",
        params![
            id,
            muxus_core::js::js_trim(&input.name),
            serde_json::to_string(&input.layout)?,
            serde_json::to_string(&groups)?
        ],
    )?;
    Ok(id)
}

/// Walk a stored layout tree defensively — its shape is unknown in the DB.
fn collect_layout_tab_ids(node: &Value, into: &mut HashSet<String>) {
    let Some(record) = node.as_object() else {
        return;
    };
    if record.get("type").and_then(Value::as_str) == Some("split")
        && let Some(children) = record.get("children").and_then(Value::as_array)
    {
        for child in children {
            collect_layout_tab_ids(child, into);
        }
        return;
    }
    let Some(tabs) = record.get("tabs").and_then(Value::as_array) else {
        return;
    };
    for tab in tabs {
        if let Some(id) = tab.get("id").and_then(Value::as_str) {
            into.insert(id.to_owned());
        }
    }
}

impl Database {
    /// Create or replace a workspace. A locked workspace only changes through
    /// an explicit save (`overwrite_locked`).
    pub fn save_workspace(
        &self,
        input: &WorkspaceInput,
        overwrite_locked: bool,
    ) -> DbResult<StoredWorkspace> {
        let conn = self.lock();
        let id = save_workspace(&conn, input, overwrite_locked)?;
        workspace(&conn, &id)?.ok_or_else(|| DbError::NotFound("workspace not found".into()))
    }

    /// Save a new workspace under the first free "Workspace N" name.
    pub fn save_workspace_with_default_name(
        &self,
        layout: Value,
        multi_exec_groups: Option<Vec<WorkspaceMultiExecGroup>>,
    ) -> DbResult<StoredWorkspace> {
        self.immediate(|conn| {
            let names: HashSet<String> = {
                let mut statement = conn.prepare("SELECT name FROM workspaces")?;
                let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
                rows.map(|name| name.map(|name| muxus_core::js::js_trim(&name).to_lowercase()))
                    .collect::<Result<_, _>>()?
            };
            let mut number = 1;
            while names.contains(&format!("workspace {number}")) {
                number += 1;
            }
            let input = WorkspaceInput {
                id: None,
                name: format!("Workspace {number}"),
                layout,
                multi_exec_groups,
            };
            let id = save_workspace(conn, &input, false)?;
            workspace(conn, &id)?.ok_or_else(|| DbError::NotFound("workspace not found".into()))
        })
    }

    pub fn workspace(&self, id: &str) -> DbResult<Option<StoredWorkspace>> {
        workspace(&self.lock(), id)
    }

    /// The most recently opened (or else updated) workspace.
    pub fn latest_workspace(&self) -> DbResult<Option<StoredWorkspace>> {
        Ok(list(
            &self.lock(),
            &format!("{WORKSPACE_COLUMNS}\n        {NEWEST_FIRST}\n        LIMIT 1"),
        )?
        .into_iter()
        .next())
    }

    /// The workspace selected for startup, if any.
    pub fn startup_workspace(&self) -> DbResult<Option<StoredWorkspace>> {
        Ok(list(
            &self.lock(),
            &format!("{WORKSPACE_COLUMNS}\n        WHERE is_startup = 1\n        LIMIT 1"),
        )?
        .into_iter()
        .next())
    }

    /// Every workspace without its layout, most recent first.
    pub fn list_workspace_summaries(&self) -> DbResult<Vec<WorkspaceSummary>> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "
        SELECT id, name, is_locked, is_startup, created_at, updated_at, last_opened_at
        FROM workspaces
        {NEWEST_FIRST}
      "
        ))?;
        let rows = statement.query_map([], |row| {
            Ok(WorkspaceSummary {
                id: row.get(0)?,
                name: row.get(1)?,
                is_locked: row.get::<_, i64>(2)? == 1,
                is_startup: row.get::<_, i64>(3)? == 1,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
                last_opened_at: optional_string(row.get(6)?),
            })
        })?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Every workspace, most recent first.
    pub fn list_workspaces(&self) -> DbResult<Vec<StoredWorkspace>> {
        list(
            &self.lock(),
            &format!("{WORKSPACE_COLUMNS}\n        {NEWEST_FIRST}"),
        )
    }

    pub fn rename_workspace(&self, id: &str, name: &str) -> DbResult<Option<StoredWorkspace>> {
        require_non_empty(name, "name")?;
        let conn = self.lock();
        let updated = conn.execute(
            "
        UPDATE workspaces
        SET name = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
            params![muxus_core::js::js_trim(name), id],
        )? > 0;
        if updated {
            workspace(&conn, id)
        } else {
            Ok(None)
        }
    }

    pub fn set_workspace_locked(
        &self,
        id: &str,
        is_locked: bool,
    ) -> DbResult<Option<StoredWorkspace>> {
        let conn = self.lock();
        let updated = conn.execute(
            "
        UPDATE workspaces
        SET is_locked = ?, updated_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
            params![is_locked, id],
        )? > 0;
        if updated {
            workspace(&conn, id)
        } else {
            Ok(None)
        }
    }

    /// Mark a workspace as just opened.
    pub fn open_workspace(&self, id: &str) -> DbResult<Option<StoredWorkspace>> {
        let conn = self.lock();
        let opened = conn.execute(
            "
        UPDATE workspaces
        SET last_opened_at = CURRENT_TIMESTAMP
        WHERE id = ?
      ",
            [id],
        )? > 0;
        if opened {
            workspace(&conn, id)
        } else {
            Ok(None)
        }
    }

    /// Select (or with `None` clear) the startup workspace; at most one is selected.
    pub fn set_startup_workspace(&self, id: Option<&str>) -> DbResult<Option<StoredWorkspace>> {
        if let Some(id) = id
            && self.workspace(id)?.is_none()
        {
            return Ok(None);
        }
        self.immediate(|conn| {
            conn.execute(
                "UPDATE workspaces SET is_startup = 0 WHERE is_startup = 1",
                [],
            )?;
            if let Some(id) = id {
                conn.execute("UPDATE workspaces SET is_startup = 1 WHERE id = ?", [id])?;
            }
            Ok(())
        })?;
        match id {
            Some(id) => self.workspace(id),
            None => Ok(None),
        }
    }

    pub fn delete_workspace(&self, id: &str) -> DbResult<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM workspaces WHERE id = ?", [id])?
            > 0)
    }

    /// Store (or replace) one tab's serialized scrollback.
    pub fn save_terminal_snapshot(
        &self,
        tab_id: &str,
        data: &str,
        format_version: u32,
    ) -> DbResult<()> {
        require_non_empty(tab_id, "tabId")?;
        self.lock().execute(
            "
        INSERT INTO terminal_snapshots(tab_id, data, format_version, updated_at)
        VALUES (?, ?, ?, CURRENT_TIMESTAMP)
        ON CONFLICT(tab_id) DO UPDATE SET
          data = excluded.data,
          format_version = excluded.format_version,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![tab_id, data, format_version],
        )?;
        Ok(())
    }

    pub fn terminal_snapshot(&self, tab_id: &str) -> DbResult<Option<TerminalSnapshotRecord>> {
        Ok(self
            .lock()
            .query_row(
                "SELECT tab_id, data, format_version, updated_at FROM terminal_snapshots WHERE tab_id = ?",
                [tab_id],
                |row| {
                    Ok(TerminalSnapshotRecord {
                        tab_id: row.get(0)?,
                        data: row.get(1)?,
                        format_version: row.get::<_, i64>(2)?.clamp(0, i64::from(u32::MAX)) as u32,
                        updated_at: row.get(3)?,
                    })
                },
            )
            .optional()?)
    }

    /// Drop snapshots for tabs no stored workspace references. Rows younger
    /// than `grace_seconds` are spared: a fresh tab's snapshot can land
    /// before its first layout autosave. The TypeScript default is 3600.
    pub fn prune_terminal_snapshots(&self, grace_seconds: u64) -> DbResult<usize> {
        let mut referenced = HashSet::new();
        for workspace in self.list_workspaces()? {
            if let Some(root) = workspace.layout.get("root") {
                collect_layout_tab_ids(root, &mut referenced);
            }
        }
        let conn = self.lock();
        let stale: Vec<String> = {
            let mut statement = conn.prepare(
                "SELECT tab_id FROM terminal_snapshots WHERE updated_at <= datetime('now', ?)",
            )?;
            let rows =
                statement.query_map([format!("-{grace_seconds} seconds")], |row| row.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        let mut pruned = 0;
        for tab_id in stale {
            if referenced.contains(&tab_id) {
                continue;
            }
            conn.execute("DELETE FROM terminal_snapshots WHERE tab_id = ?", [&tab_id])?;
            pruned += 1;
        }
        Ok(pruned)
    }
}

/// The grace period `pruneTerminalSnapshots()` uses by default.
pub const DEFAULT_SNAPSHOT_GRACE_SECONDS: u64 = 3600;

impl Database {
    /// The storage side of `PUT /api/workspaces` for a validated request:
    /// a save never renames an existing workspace (a stale window must not
    /// overwrite the authoritative name), an id that no longer exists is
    /// [`DbError::NotFound`] unless `create_if_missing` (which never replaces
    /// an existing workspace), `allocate_default_name` picks the first free
    /// "Workspace N", and a locked workspace changes only with
    /// `overwrite_locked`. Snapshots no workspace references any more are
    /// pruned afterwards, as the route does.
    pub fn put_workspace(
        &self,
        request: &muxus_core::api::WorkspaceSaveRequest,
    ) -> DbResult<StoredWorkspace> {
        let layout = serde_json::to_value(&request.layout)?;
        let groups = Some(request.multi_exec_groups.clone());
        let existing = match &request.id {
            Some(id) => self.workspace(id)?,
            None => None,
        };
        if request.id.is_some() && existing.is_none() && !request.create_if_missing {
            return Err(DbError::NotFound("workspace not found".into()));
        }
        let saved = if request.allocate_default_name {
            self.save_workspace_with_default_name(layout, groups)?
        } else if let (Some(existing), true) = (&existing, request.create_if_missing) {
            existing.clone()
        } else {
            let name = existing
                .as_ref()
                .map_or_else(|| request.name.clone(), |existing| existing.name.clone());
            let input = WorkspaceInput {
                id: request.id.clone(),
                name,
                layout,
                multi_exec_groups: groups,
            };
            self.save_workspace(&input, request.overwrite_locked)?
        };
        self.prune_terminal_snapshots(DEFAULT_SNAPSHOT_GRACE_SECONDS)?;
        Ok(saved)
    }
}
