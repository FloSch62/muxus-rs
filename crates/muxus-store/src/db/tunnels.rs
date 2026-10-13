//! Saved tunnels.

use muxus_core::api::{ForwardType, TunnelInput, TunnelRecord};
use rusqlite::{OptionalExtension, Row, params};

use super::{
    Database, DbError, DbResult, assert_secret_free, optional_string, require_non_empty,
    trimmed_or_none,
};
use crate::ids::nanoid;

const TUNNEL_COLUMNS: &str = "
        SELECT id, name, target, ssh_options_json, type, bind_port, target_host, target_port, auto_start, auto_reconnect, created_at, updated_at
        FROM tunnels";

fn tunnel_from_row(row: &Row<'_>) -> rusqlite::Result<DbResult<TunnelRecord>> {
    let kind: String = row.get("type")?;
    let ssh_options: Option<String> = row.get("ssh_options_json")?;
    let id: String = row.get("id")?;
    let name = optional_string(row.get("name")?);
    let target: String = row.get("target")?;
    let bind_port: i64 = row.get("bind_port")?;
    let target_host: Option<String> = row.get("target_host")?;
    let target_port: Option<i64> = row.get("target_port")?;
    let auto_start = row.get::<_, i64>("auto_start")? == 1;
    let auto_reconnect = row.get::<_, i64>("auto_reconnect")? == 1;
    let created_at: String = row.get("created_at")?;
    let updated_at: String = row.get("updated_at")?;
    Ok((|| {
        let kind = ForwardType::parse(&kind)
            .ok_or_else(|| DbError::Invalid(format!("unknown tunnel type {kind}")))?;
        let dynamic = kind == ForwardType::Dynamic;
        Ok(TunnelRecord {
            id,
            name,
            target,
            ssh_options: ssh_options
                .map(|json| serde_json::from_str(&json))
                .transpose()?,
            kind,
            bind_port: u16::try_from(bind_port).unwrap_or_default(),
            target_host: if dynamic { None } else { target_host },
            target_port: if dynamic {
                None
            } else {
                target_port.and_then(|port| u16::try_from(port).ok())
            },
            auto_start,
            auto_reconnect,
            created_at,
            updated_at,
        })
    })())
}

impl Database {
    /// Every saved tunnel by display name (or target), then age.
    pub fn list_tunnels(&self) -> DbResult<Vec<TunnelRecord>> {
        let conn = self.lock();
        let mut statement = conn.prepare(&format!(
            "{TUNNEL_COLUMNS}\n        ORDER BY COALESCE(NULLIF(name, ''), target) COLLATE NOCASE, created_at"
        ))?;
        let rows = statement.query_map([], tunnel_from_row)?;
        let mut tunnels = Vec::new();
        for row in rows {
            tunnels.push(row??);
        }
        Ok(tunnels)
    }

    /// Create or replace a saved tunnel. Absent switches are off.
    pub fn save_tunnel(&self, input: &TunnelInput) -> DbResult<TunnelRecord> {
        require_non_empty(&input.target, "target")?;
        let ssh_options = input
            .ssh_options
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?;
        if let Some(options) = &ssh_options {
            assert_secret_free(options, "tunnel.sshOptions")?;
        }
        let dynamic = input.kind == ForwardType::Dynamic;
        let target_host = trimmed_or_none(input.target_host.as_deref());
        if !dynamic && (target_host.is_none() || input.target_port.is_none_or(|port| port == 0)) {
            return Err(DbError::Invalid(
                "targetHost and targetPort are required for local/remote tunnels".into(),
            ));
        }
        let id = input.id.clone().unwrap_or_else(nanoid);
        self.lock().execute(
            "
        INSERT INTO tunnels(id, name, target, ssh_options_json, type, bind_port, target_host, target_port, auto_start, auto_reconnect)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
        ON CONFLICT(id) DO UPDATE SET
          name = excluded.name,
          target = excluded.target,
          ssh_options_json = excluded.ssh_options_json,
          type = excluded.type,
          bind_port = excluded.bind_port,
          target_host = excluded.target_host,
          target_port = excluded.target_port,
          auto_start = excluded.auto_start,
          auto_reconnect = excluded.auto_reconnect,
          updated_at = CURRENT_TIMESTAMP
      ",
            params![
                id,
                trimmed_or_none(input.name.as_deref()),
                muxus_core::js::js_trim(&input.target),
                ssh_options.as_ref().map(serde_json::to_string).transpose()?,
                input.kind.as_str(),
                input.bind_port,
                if dynamic { None } else { target_host },
                if dynamic { None } else { input.target_port },
                input.auto_start.unwrap_or(false),
                input.auto_reconnect.unwrap_or(false),
            ],
        )?;
        self.tunnel(&id)?
            .ok_or_else(|| DbError::NotFound("tunnel not found".into()))
    }

    pub fn tunnel(&self, id: &str) -> DbResult<Option<TunnelRecord>> {
        self.lock()
            .query_row(
                &format!("{TUNNEL_COLUMNS} WHERE id = ?"),
                [id],
                tunnel_from_row,
            )
            .optional()?
            .transpose()
    }

    pub fn delete_tunnel(&self, id: &str) -> DbResult<bool> {
        Ok(self
            .lock()
            .execute("DELETE FROM tunnels WHERE id = ?", [id])?
            > 0)
    }
}
