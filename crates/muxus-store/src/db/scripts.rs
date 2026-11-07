//! The script library: scripts kept in Muxus to run on many hosts
//! (`listScripts`, `saveScript`, `deleteScript` in database.ts). The table's
//! CHECKs bound the name (1–200 characters) and the body (1 MiB); callers
//! validate [`SavedScriptInput`] against the route's limits first.

use muxus_core::api::{SavedScript, SavedScriptInput};
use rusqlite::{Row, params};

use super::{Database, DbResult, require_non_empty};
use crate::ids::nanoid;

fn script_from_row(row: &Row<'_>) -> rusqlite::Result<SavedScript> {
    Ok(SavedScript {
        id: row.get("id")?,
        name: row.get("name")?,
        body: row.get("body")?,
        created_at: row.get("created_at")?,
        updated_at: row.get("updated_at")?,
    })
}

impl Database {
    /// Every saved script, by name.
    pub fn list_scripts(&self) -> DbResult<Vec<SavedScript>> {
        let conn = self.lock();
        let mut statement =
            conn.prepare("SELECT * FROM scripts ORDER BY name COLLATE NOCASE, created_at")?;
        let rows = statement.query_map([], script_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Create a script, or replace the one with the input's id.
    pub fn save_script(&self, input: &SavedScriptInput) -> DbResult<SavedScript> {
        require_non_empty(&input.name, "name")?;
        let id = input.id.clone().unwrap_or_else(nanoid);
        let conn = self.lock();
        conn.execute(
            "
        INSERT INTO scripts(id, name, body) VALUES (?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET name = excluded.name, body = excluded.body, updated_at = CURRENT_TIMESTAMP
      ",
            params![id, muxus_core::js::js_trim(&input.name), input.body],
        )?;
        Ok(conn.query_row("SELECT * FROM scripts WHERE id = ?", [&id], script_from_row)?)
    }

    /// Delete a script; whether there was one.
    pub fn delete_script(&self, id: &str) -> DbResult<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM scripts WHERE id = ?", [id])?
            > 0)
    }
}
