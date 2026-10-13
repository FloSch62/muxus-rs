//! Schema migrations, run exactly as the TypeScript `migrate()` runs them.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use super::migration_sql as sql;
use super::{DbError, DbResult};

/// The newest schema version this build knows (the last TypeScript migration).
pub const SUPPORTED_SCHEMA_VERSION: i64 = 29;

/// One row of `schema_migrations`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedMigration {
    pub version: i64,
    pub name: String,
}

enum Step {
    Sql(&'static str),
    Run(fn(&Connection) -> DbResult<()>),
}

struct Migration {
    version: i64,
    name: &'static str,
    step: Step,
}

const MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        name: "foundation",
        step: Step::Sql(sql::MIGRATION_1),
    },
    Migration {
        version: 2,
        name: "tunnels",
        step: Step::Sql(sql::MIGRATION_2),
    },
    Migration {
        version: 3,
        name: "host-sort-order",
        step: Step::Sql(sql::MIGRATION_3),
    },
    Migration {
        version: 4,
        name: "tunnel-ssh-options",
        step: Step::Sql(sql::MIGRATION_4),
    },
    Migration {
        version: 5,
        name: "host-keyword-highlights",
        step: Step::Sql(sql::MIGRATION_5),
    },
    Migration {
        version: 6,
        name: "persistent-session-history",
        step: Step::Sql(sql::MIGRATION_6),
    },
    Migration {
        version: 7,
        name: "bounded-session-history-settings",
        step: Step::Sql(sql::MIGRATION_7),
    },
    Migration {
        version: 8,
        name: "named-workspace-session-sets",
        step: Step::Sql(sql::MIGRATION_8),
    },
    Migration {
        version: 9,
        name: "drop-favorites",
        step: Step::Sql(sql::MIGRATION_9),
    },
    Migration {
        version: 10,
        name: "terminal-scrollback-snapshots",
        step: Step::Sql(sql::MIGRATION_10),
    },
    Migration {
        version: 11,
        name: "version-terminal-scrollback-snapshots",
        step: Step::Sql(sql::MIGRATION_11),
    },
    Migration {
        version: 12,
        name: "password-vault",
        step: Step::Sql(sql::MIGRATION_12),
    },
    // Version 13 existed briefly on the feature branch. Keep the number
    // reserved so fresh databases and databases created by that draft can
    // converge on the same migration history.
    Migration {
        version: 13,
        name: "automatic-password-vault",
        step: Step::Sql(sql::MIGRATION_13),
    },
    Migration {
        version: 14,
        name: "password-vault-os-keystore",
        step: Step::Run(migrate_draft_password_vault),
    },
    Migration {
        version: 15,
        name: "password-vault-key-check",
        step: Step::Run(add_password_vault_key_check),
    },
    Migration {
        version: 16,
        name: "password-vault-key-cleanup",
        step: Step::Sql(sql::MIGRATION_16),
    },
    Migration {
        version: 17,
        name: "folder-settings",
        step: Step::Sql(sql::MIGRATION_17),
    },
    Migration {
        version: 18,
        name: "lock-workspaces",
        step: Step::Sql(sql::MIGRATION_18),
    },
    Migration {
        version: 19,
        name: "host-disable-sftp",
        step: Step::Sql(sql::MIGRATION_19),
    },
    Migration {
        version: 20,
        name: "host-console-compatibility",
        step: Step::Sql(sql::MIGRATION_20),
    },
    Migration {
        version: 21,
        name: "host-terminal-appearance",
        step: Step::Sql(sql::MIGRATION_21),
    },
    Migration {
        version: 22,
        name: "remote-desktop-hosts",
        step: Step::Run(add_remote_desktop_hosts),
    },
    Migration {
        version: 23,
        name: "session-log-files",
        step: Step::Sql(sql::MIGRATION_23),
    },
    Migration {
        version: 24,
        name: "host-command-button-group",
        step: Step::Sql(sql::MIGRATION_24),
    },
    Migration {
        version: 25,
        name: "tunnel-autostart-reconnect",
        step: Step::Sql(sql::MIGRATION_25),
    },
    // Named vault secrets carry an optional user name as plain metadata.
    Migration {
        version: 26,
        name: "credential-usernames",
        step: Step::Sql(sql::MIGRATION_26),
    },
    // NULL inherits the folder's sequence; an empty step list means none.
    Migration {
        version: 27,
        name: "login-sequences",
        step: Step::Sql(sql::MIGRATION_27),
    },
    // NULL follows the Terminal settings; the upper bounds are the API's.
    Migration {
        version: 28,
        name: "host-paste-pacing",
        step: Step::Sql(sql::MIGRATION_28),
    },
    Migration {
        version: 29,
        name: "management-hosts",
        step: Step::Run(add_management_hosts),
    },
];

const SCHEMA_MIGRATIONS_SQL: &str = "
      CREATE TABLE IF NOT EXISTS schema_migrations (
        version INTEGER PRIMARY KEY,
        name TEXT NOT NULL UNIQUE,
        applied_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;
    ";

/// Create `schema_migrations`, refuse a newer schema, then apply every
/// missing migration in its own `BEGIN IMMEDIATE` transaction.
pub(super) fn migrate(conn: &Connection) -> DbResult<()> {
    conn.execute_batch(SCHEMA_MIGRATIONS_SQL)?;
    let applied: Vec<i64> = {
        let mut statement = conn.prepare("SELECT version FROM schema_migrations")?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    let newest = applied.iter().copied().max().unwrap_or(0).max(0);
    if newest > SUPPORTED_SCHEMA_VERSION {
        return Err(DbError::NewerSchema {
            found: newest,
            supported: SUPPORTED_SCHEMA_VERSION,
        });
    }
    for migration in MIGRATIONS {
        if applied.contains(&migration.version) {
            continue;
        }
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let result = (|| -> DbResult<()> {
            match migration.step {
                Step::Sql(text) => conn.execute_batch(text)?,
                Step::Run(run) => run(conn)?,
            }
            conn.execute(
                "INSERT INTO schema_migrations(version, name) VALUES (?, ?)",
                params![migration.version, migration.name],
            )?;
            conn.execute_batch(&format!("PRAGMA user_version = {}", migration.version))?;
            conn.execute_batch("COMMIT")?;
            Ok(())
        })();
        if let Err(error) = result {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(error);
        }
    }
    Ok(())
}

pub(super) fn applied_migrations(conn: &Connection) -> DbResult<Vec<AppliedMigration>> {
    let mut statement =
        conn.prepare("SELECT version, name FROM schema_migrations ORDER BY version")?;
    let rows = statement.query_map([], |row| {
        Ok(AppliedMigration {
            version: row.get(0)?,
            name: row.get(1)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn table_columns(conn: &Connection, table: &str) -> DbResult<Vec<String>> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>("name"))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Version 14: a draft version-13 vault (with a device-key copy) becomes a
/// startup-policy vault with a fresh vault id; other databases are untouched.
fn migrate_draft_password_vault(conn: &Connection) -> DbResult<()> {
    if !table_columns(conn, "password_vault")?
        .iter()
        .any(|name| name == "device_key_nonce")
    {
        return Ok(());
    }
    conn.execute_batch(sql::DRAFT_VAULT_SQL)?;
    conn.execute(sql::DRAFT_VAULT_COPY_SQL, params![crate::ids::nanoid()])?;
    conn.execute_batch("DROP TABLE password_vault_draft_v13")?;
    Ok(())
}

/// Version 15: the key-check column, unless a draft already added it.
fn add_password_vault_key_check(conn: &Connection) -> DbResult<()> {
    if table_columns(conn, "password_vault")?
        .iter()
        .any(|name| name == "key_check")
    {
        return Ok(());
    }
    conn.execute_batch(sql::KEY_CHECK_SQL)?;
    Ok(())
}

/// Version 22: RDP and VNC hosts need their kinds admitted by the
/// connection_profiles CHECK constraint, which SQLite can only change by
/// rebuilding the table. The rebuild reuses the table's own stored definition
/// so every column added by earlier migrations carries over unchanged.
fn add_remote_desktop_hosts(conn: &Connection) -> DbResult<()> {
    rebuild_connection_profile_kinds(
        conn,
        sql::REMOTE_DESKTOP_PREVIOUS_KINDS,
        sql::REMOTE_DESKTOP_NEXT_KINDS,
        "RDP and VNC hosts",
    )?;
    conn.execute_batch(sql::REMOTE_DESKTOP_SQL)?;
    Ok(())
}

/// Version 29: gNMI and NETCONF hosts join the saved host kinds, and their
/// saved requests get a table. gNMI certificates are pinned in
/// remote_desktop_certificates, keyed like RDP's by host, port and route.
fn add_management_hosts(conn: &Connection) -> DbResult<()> {
    rebuild_connection_profile_kinds(
        conn,
        sql::MANAGEMENT_PREVIOUS_KINDS,
        sql::MANAGEMENT_NEXT_KINDS,
        "gNMI and NETCONF hosts",
    )?;
    conn.execute_batch(sql::MANAGEMENT_SQL)?;
    Ok(())
}

/// Admit new kinds in the connection_profiles CHECK constraint by rebuilding
/// the table from its own stored definition.
fn rebuild_connection_profile_kinds(
    conn: &Connection,
    previous: &str,
    next: &str,
    what: &str,
) -> DbResult<()> {
    let definition: String = conn
        .query_row(sql::REBUILD_TABLE_SQL, [], |row| {
            row.get::<_, Option<String>>(0)
        })
        .optional()?
        .flatten()
        .unwrap_or_default();
    if !definition.contains(previous) {
        return Err(DbError::Invalid(format!(
            "connection_profiles has an unexpected definition; cannot add {what}"
        )));
    }
    let rebuilt = rename_create_table(&definition.replacen(previous, next, 1));
    let indexes: Vec<String> = {
        let mut statement = conn.prepare(sql::REBUILD_INDEXES_SQL)?;
        statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<_, _>>()?
    };
    // Foreign keys stay on inside the migration transaction, so dropping the
    // old table cascades into connection_tags; keep those rows aside.
    conn.execute_batch(&format!(
        "{}{}{}",
        sql::REBUILD_BEFORE_SQL,
        rebuilt,
        sql::REBUILD_AFTER_SQL
    ))?;
    for index in indexes {
        conn.execute_batch(&index)?;
    }
    conn.execute_batch(sql::REBUILD_RESTORE_TAGS_SQL)?;
    Ok(())
}

/// `.replace(/^CREATE TABLE\s+"?connection_profiles"?/, 'CREATE TABLE connection_profiles_rebuilt')`.
fn rename_create_table(definition: &str) -> String {
    let Some(rest) = definition.strip_prefix("CREATE TABLE") else {
        return definition.to_owned();
    };
    let after_space = rest.trim_start_matches(|c: char| c.is_whitespace());
    if after_space.len() == rest.len() {
        return definition.to_owned();
    }
    let unquoted = after_space.strip_prefix('"').unwrap_or(after_space);
    let Some(tail) = unquoted.strip_prefix("connection_profiles") else {
        return definition.to_owned();
    };
    let tail = tail.strip_prefix('"').unwrap_or(tail);
    format!("CREATE TABLE connection_profiles_rebuilt{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renames_the_stored_create_statement() {
        assert_eq!(
            rename_create_table("CREATE TABLE \"connection_profiles\" (id TEXT)"),
            "CREATE TABLE connection_profiles_rebuilt (id TEXT)"
        );
        assert_eq!(
            rename_create_table("CREATE TABLE connection_profiles (\n id TEXT)"),
            "CREATE TABLE connection_profiles_rebuilt (\n id TEXT)"
        );
    }
}
