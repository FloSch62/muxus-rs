//! The application database migration SQL, verbatim from the TypeScript build.
//!
//! SQLite keeps the text of every CREATE statement (and splices each ALTER
//! TABLE ADD COLUMN definition into it), so these strings are copied byte for
//! byte from `server/src/persistence/database.ts`: a fresh database then has
//! exactly the schema text the TypeScript build creates. Do not reformat.

/// Migration 1, `foundation`.
pub(super) const MIGRATION_1: &str = r#"
      CREATE TABLE credential_refs (
        id TEXT PRIMARY KEY,
        provider TEXT NOT NULL,
        service TEXT NOT NULL,
        account TEXT NOT NULL,
        label TEXT,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        UNIQUE(provider, service, account)
      ) STRICT;

      CREATE TABLE connection_groups (
        id TEXT PRIMARY KEY,
        parent_id TEXT REFERENCES connection_groups(id) ON DELETE CASCADE,
        name TEXT NOT NULL,
        sort_order INTEGER NOT NULL DEFAULT 0,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;

      CREATE TABLE connection_profiles (
        id TEXT PRIMARY KEY,
        kind TEXT NOT NULL CHECK(kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet')),
        name TEXT NOT NULL,
        ssh_alias TEXT UNIQUE,
        native_config_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(native_config_json)),
        credential_ref_id TEXT REFERENCES credential_refs(id) ON DELETE SET NULL,
        group_id TEXT REFERENCES connection_groups(id) ON DELETE SET NULL,
        favorite INTEGER NOT NULL DEFAULT 0 CHECK(favorite IN (0, 1)),
        color TEXT,
        icon TEXT,
        last_connected_at TEXT,
        connect_count INTEGER NOT NULL DEFAULT 0 CHECK(connect_count >= 0),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        CHECK(
          (kind = 'openssh' AND ssh_alias IS NOT NULL AND native_config_json = '{}')
          OR (kind <> 'openssh' AND ssh_alias IS NULL)
        )
      ) STRICT;

      CREATE TABLE tags (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL COLLATE NOCASE UNIQUE,
        color TEXT,
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;

      CREATE TABLE connection_tags (
        connection_id TEXT NOT NULL REFERENCES connection_profiles(id) ON DELETE CASCADE,
        tag_id TEXT NOT NULL REFERENCES tags(id) ON DELETE CASCADE,
        PRIMARY KEY(connection_id, tag_id)
      ) STRICT, WITHOUT ROWID;

      CREATE TABLE workspaces (
        id TEXT PRIMARY KEY,
        name TEXT NOT NULL,
        layout_json TEXT NOT NULL CHECK(json_valid(layout_json)),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        last_opened_at TEXT
      ) STRICT;

      CREATE INDEX connection_profiles_recent
        ON connection_profiles(last_connected_at DESC)
        WHERE last_connected_at IS NOT NULL;
      CREATE INDEX connection_profiles_group
        ON connection_profiles(group_id, name COLLATE NOCASE);
    "#;

/// Migration 2, `tunnels`.
pub(super) const MIGRATION_2: &str = r#"
      CREATE TABLE tunnels (
        id TEXT PRIMARY KEY,
        name TEXT,
        target TEXT NOT NULL,
        type TEXT NOT NULL CHECK(type IN ('local', 'remote', 'dynamic')),
        bind_port INTEGER NOT NULL CHECK(bind_port BETWEEN 1 AND 65535),
        target_host TEXT,
        target_port INTEGER CHECK(target_port BETWEEN 1 AND 65535),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        CHECK(
          (type = 'dynamic' AND target_host IS NULL AND target_port IS NULL)
          OR (type <> 'dynamic' AND target_host IS NOT NULL AND target_port IS NOT NULL)
        )
      ) STRICT;
    "#;

/// Migration 3, `host-sort-order`.
pub(super) const MIGRATION_3: &str = r#"
      ALTER TABLE connection_profiles ADD COLUMN sort_order INTEGER;
    "#;

/// Migration 4, `tunnel-ssh-options`.
pub(super) const MIGRATION_4: &str = r#"
      ALTER TABLE tunnels
        ADD COLUMN ssh_options_json TEXT CHECK(ssh_options_json IS NULL OR json_valid(ssh_options_json));
    "#;

/// Migration 5, `host-keyword-highlights`.
pub(super) const MIGRATION_5: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN keyword_highlights_json TEXT
        CHECK(keyword_highlights_json IS NULL OR json_valid(keyword_highlights_json));
    "#;

/// Migration 6, `persistent-session-history`.
pub(super) const MIGRATION_6: &str = r#"
      CREATE TABLE session_logging_policies (
        profile_key TEXT PRIMARY KEY,
        enabled INTEGER NOT NULL CHECK(enabled IN (0, 1)),
        capture_input INTEGER NOT NULL CHECK(capture_input IN (0, 1)),
        max_part_bytes INTEGER NOT NULL CHECK(max_part_bytes BETWEEN 65536 AND 1073741824),
        max_parts INTEGER NOT NULL CHECK(max_parts BETWEEN 1 AND 1000),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;

      CREATE TABLE session_logs (
        id TEXT PRIMARY KEY,
        profile_key TEXT NOT NULL,
        title TEXT NOT NULL,
        kind TEXT NOT NULL CHECK(kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet')),
        host TEXT NOT NULL,
        started_at TEXT NOT NULL,
        ended_at TEXT,
        status TEXT NOT NULL CHECK(status IN ('active', 'completed', 'disconnected', 'failed')),
        paused INTEGER NOT NULL DEFAULT 0 CHECK(paused IN (0, 1)),
        capture_input INTEGER NOT NULL CHECK(capture_input IN (0, 1)),
        event_count INTEGER NOT NULL DEFAULT 0 CHECK(event_count >= 0),
        raw_bytes INTEGER NOT NULL DEFAULT 0 CHECK(raw_bytes >= 0),
        normalized_bytes INTEGER NOT NULL DEFAULT 0 CHECK(normalized_bytes >= 0),
        current_part INTEGER NOT NULL DEFAULT 1 CHECK(current_part >= 1),
        current_part_bytes INTEGER NOT NULL DEFAULT 0 CHECK(current_part_bytes >= 0)
      ) STRICT;

      CREATE TABLE session_log_events (
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        session_id TEXT NOT NULL REFERENCES session_logs(id) ON DELETE CASCADE,
        part_number INTEGER NOT NULL CHECK(part_number >= 1),
        sequence INTEGER NOT NULL CHECK(sequence >= 1),
        recorded_at TEXT NOT NULL,
        elapsed_ms INTEGER NOT NULL CHECK(elapsed_ms >= 0),
        direction TEXT NOT NULL CHECK(direction IN ('input', 'output', 'system')),
        raw_data BLOB NOT NULL,
        normalized_text TEXT NOT NULL,
        UNIQUE(session_id, sequence)
      ) STRICT;

      CREATE INDEX session_logs_started ON session_logs(started_at DESC);
      CREATE INDEX session_logs_profile_started ON session_logs(profile_key, started_at DESC);
      CREATE INDEX session_log_events_session_sequence
        ON session_log_events(session_id, sequence);
      CREATE INDEX session_log_events_session_part
        ON session_log_events(session_id, part_number);

      CREATE VIRTUAL TABLE session_log_events_fts USING fts5(
        normalized_text,
        content = 'session_log_events',
        content_rowid = 'id',
        tokenize = 'unicode61'
      );

      CREATE TRIGGER session_log_events_fts_insert AFTER INSERT ON session_log_events BEGIN
        INSERT INTO session_log_events_fts(rowid, normalized_text)
        VALUES (new.id, new.normalized_text);
      END;
      CREATE TRIGGER session_log_events_fts_delete AFTER DELETE ON session_log_events BEGIN
        INSERT INTO session_log_events_fts(session_log_events_fts, rowid, normalized_text)
        VALUES ('delete', old.id, old.normalized_text);
      END;
      CREATE TRIGGER session_log_events_fts_update AFTER UPDATE ON session_log_events BEGIN
        INSERT INTO session_log_events_fts(session_log_events_fts, rowid, normalized_text)
        VALUES ('delete', old.id, old.normalized_text);
        INSERT INTO session_log_events_fts(rowid, normalized_text)
        VALUES (new.id, new.normalized_text);
      END;
    "#;

/// Migration 7, `bounded-session-history-settings`.
pub(super) const MIGRATION_7: &str = r#"
      CREATE TABLE session_history_settings (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        storage_location TEXT,
        max_total_bytes INTEGER NOT NULL CHECK(max_total_bytes >= 67108864),
        min_free_bytes INTEGER NOT NULL CHECK(min_free_bytes >= 0),
        min_free_percent REAL NOT NULL CHECK(min_free_percent BETWEEN 0 AND 100),
        max_age_days INTEGER CHECK(max_age_days IS NULL OR max_age_days >= 1),
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;

      INSERT INTO session_history_settings(
        singleton, storage_location, max_total_bytes, min_free_bytes,
        min_free_percent, max_age_days
      ) VALUES (1, NULL, 5368709120, 2147483648, 5, NULL);
    "#;

/// Migration 8, `named-workspace-session-sets`.
pub(super) const MIGRATION_8: &str = r#"
      ALTER TABLE workspaces
        ADD COLUMN multi_exec_groups_json TEXT NOT NULL DEFAULT '[]'
        CHECK(json_valid(multi_exec_groups_json));
      ALTER TABLE workspaces
        ADD COLUMN is_startup INTEGER NOT NULL DEFAULT 0
        CHECK(is_startup IN (0, 1));
      CREATE UNIQUE INDEX workspaces_single_startup
        ON workspaces(is_startup)
        WHERE is_startup = 1;
    "#;

/// Migration 9, `drop-favorites`.
pub(super) const MIGRATION_9: &str = r#"
      ALTER TABLE connection_profiles DROP COLUMN favorite;
    "#;

/// Migration 10, `terminal-scrollback-snapshots`.
pub(super) const MIGRATION_10: &str = r#"
      CREATE TABLE terminal_snapshots (
        tab_id TEXT PRIMARY KEY,
        data TEXT NOT NULL,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      );
    "#;

/// Migration 11, `version-terminal-scrollback-snapshots`.
pub(super) const MIGRATION_11: &str = r#"
      ALTER TABLE terminal_snapshots
        ADD COLUMN format_version INTEGER NOT NULL DEFAULT 1
        CHECK(format_version >= 1);
    "#;

/// Migration 12, `password-vault`.
pub(super) const MIGRATION_12: &str = r#"
      CREATE TABLE password_vault (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        format_version INTEGER NOT NULL CHECK(format_version IN (2, 3)),
        vault_id TEXT NOT NULL UNIQUE CHECK(length(vault_id) BETWEEN 16 AND 64),
        unlock_policy TEXT NOT NULL
          CHECK(unlock_policy IN ('never', 'startup', 'credential')),
        kdf_algorithm TEXT NOT NULL CHECK(kdf_algorithm = 'scrypt'),
        kdf_salt BLOB NOT NULL CHECK(length(kdf_salt) = 16),
        kdf_cost INTEGER NOT NULL CHECK(kdf_cost BETWEEN 1024 AND 1048576),
        kdf_block_size INTEGER NOT NULL CHECK(kdf_block_size BETWEEN 1 AND 32),
        kdf_parallelism INTEGER NOT NULL CHECK(kdf_parallelism BETWEEN 1 AND 16),
        master_key_nonce BLOB NOT NULL CHECK(length(master_key_nonce) = 12),
        master_key_ciphertext BLOB NOT NULL CHECK(length(master_key_ciphertext) = 32),
        master_key_tag BLOB NOT NULL CHECK(length(master_key_tag) = 16),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;

      CREATE TABLE credential_secrets (
        credential_ref_id TEXT PRIMARY KEY
          REFERENCES credential_refs(id) ON DELETE CASCADE,
        format_version INTEGER NOT NULL CHECK(format_version = 1),
        nonce BLOB NOT NULL CHECK(length(nonce) = 12),
        ciphertext BLOB NOT NULL,
        auth_tag BLOB NOT NULL CHECK(length(auth_tag) = 16),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;
    "#;

/// Migration 13, `automatic-password-vault`.
pub(super) const MIGRATION_13: &str = r#"SELECT 1;"#;

/// Migration 16, `password-vault-key-cleanup`.
pub(super) const MIGRATION_16: &str = r#"
      CREATE TABLE password_vault_key_cleanup (
        vault_id TEXT PRIMARY KEY CHECK(length(vault_id) BETWEEN 16 AND 64),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;
    "#;

/// Migration 17, `folder-settings`.
pub(super) const MIGRATION_17: &str = r#"
      CREATE TABLE folder_settings (
        id TEXT PRIMARY KEY,
        path_key TEXT NOT NULL UNIQUE,
        path TEXT NOT NULL,
        auth_json TEXT NOT NULL DEFAULT '{}' CHECK(json_valid(auth_json)),
        created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;
    "#;

/// Migration 18, `lock-workspaces`.
pub(super) const MIGRATION_18: &str = r#"
      ALTER TABLE workspaces
        ADD COLUMN is_locked INTEGER NOT NULL DEFAULT 0
        CHECK(is_locked IN (0, 1));
    "#;

/// Migration 19, `host-disable-sftp`.
pub(super) const MIGRATION_19: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN disable_sftp INTEGER NOT NULL DEFAULT 0
        CHECK(disable_sftp IN (0, 1));
    "#;

/// Migration 20, `host-console-compatibility`.
pub(super) const MIGRATION_20: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN console_compatibility INTEGER NOT NULL DEFAULT 0
        CHECK(console_compatibility IN (0, 1));
    "#;

/// Migration 21, `host-terminal-appearance`.
pub(super) const MIGRATION_21: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN terminal_scheme TEXT;
      ALTER TABLE connection_profiles
        ADD COLUMN terminal_font_color TEXT;
      ALTER TABLE connection_profiles
        ADD COLUMN terminal_background_color TEXT;
    "#;

/// Migration 23, `session-log-files`.
pub(super) const MIGRATION_23: &str = r#"
      ALTER TABLE session_logging_policies
        ADD COLUMN log_to_file INTEGER NOT NULL DEFAULT 0
        CHECK(log_to_file IN (0, 1));

      CREATE TABLE session_log_file_settings (
        singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
        directory TEXT,
        filename_pattern TEXT NOT NULL CHECK(length(filename_pattern) BETWEEN 1 AND 200),
        timestamps INTEGER NOT NULL CHECK(timestamps IN (0, 1)),
        updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
      ) STRICT;

      INSERT INTO session_log_file_settings(singleton, directory, filename_pattern, timestamps)
      VALUES (1, NULL, '{host}_{date}_{time}.log', 0);
    "#;

/// Migration 24, `host-command-button-group`.
pub(super) const MIGRATION_24: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN command_button_group TEXT;
    "#;

/// Migration 25, `tunnel-autostart-reconnect`.
pub(super) const MIGRATION_25: &str = r#"
      ALTER TABLE tunnels
        ADD COLUMN auto_start INTEGER NOT NULL DEFAULT 0 CHECK(auto_start IN (0, 1));
      ALTER TABLE tunnels
        ADD COLUMN auto_reconnect INTEGER NOT NULL DEFAULT 0 CHECK(auto_reconnect IN (0, 1));
    "#;

/// Migration 26, `credential-usernames`.
pub(super) const MIGRATION_26: &str = r#"
      ALTER TABLE credential_refs
        ADD COLUMN username TEXT;
    "#;

/// Migration 27, `login-sequences`.
pub(super) const MIGRATION_27: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN login_sequence_json TEXT
        CHECK(login_sequence_json IS NULL OR json_valid(login_sequence_json));
      ALTER TABLE folder_settings
        ADD COLUMN login_sequence_json TEXT
        CHECK(login_sequence_json IS NULL OR json_valid(login_sequence_json));
    "#;

/// Migration 28, `host-paste-pacing`.
pub(super) const MIGRATION_28: &str = r#"
      ALTER TABLE connection_profiles
        ADD COLUMN paste_line_delay_ms INTEGER
        CHECK(paste_line_delay_ms IS NULL OR paste_line_delay_ms >= 0);
      ALTER TABLE connection_profiles
        ADD COLUMN paste_char_delay_ms INTEGER
        CHECK(paste_char_delay_ms IS NULL OR paste_char_delay_ms >= 0);
    "#;

/// Migration 22: the kinds connection_profiles admitted before.
pub(super) const REMOTE_DESKTOP_PREVIOUS_KINDS: &str =
    r#"kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet')"#;

/// Migration 22: the kinds it admits after.
pub(super) const REMOTE_DESKTOP_NEXT_KINDS: &str =
    r#"kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet', 'rdp', 'vnc')"#;

/// Migration 22: the certificate store.
pub(super) const REMOTE_DESKTOP_SQL: &str = r#"
    -- Pinned RDP certificates, and VNC RSA-AES keys, per host, port and route.
    CREATE TABLE remote_desktop_certificates (
      host TEXT NOT NULL,
      port INTEGER NOT NULL CHECK(port BETWEEN 1 AND 65535),
      gateway TEXT NOT NULL DEFAULT '',
      fingerprint TEXT NOT NULL,
      subject TEXT NOT NULL DEFAULT '',
      created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
      updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
      PRIMARY KEY(host, port, gateway)
    ) STRICT;
  "#;

/// Migration 29: the kinds connection_profiles admitted before.
pub(super) const MANAGEMENT_PREVIOUS_KINDS: &str =
    r#"kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet', 'rdp', 'vnc')"#;

/// Migration 29: the kinds it admits after.
pub(super) const MANAGEMENT_NEXT_KINDS: &str =
    r#"kind IN ('openssh', 'ssh', 'local', 'serial', 'telnet', 'rdp', 'vnc', 'gnmi', 'netconf')"#;

/// Migration 29: saved management requests.
pub(super) const MANAGEMENT_SQL: &str = r#"
    -- No foreign key: a later rebuild of connection_profiles would cascade
    -- into it. Deleting a saved host deletes its requests explicitly.
    CREATE TABLE management_requests (
      id TEXT PRIMARY KEY,
      protocol TEXT NOT NULL CHECK(protocol IN ('gnmi', 'netconf')),
      -- NULL: offered for every host of the protocol.
      profile_id TEXT,
      name TEXT NOT NULL CHECK(length(name) BETWEEN 1 AND 200),
      request_json TEXT NOT NULL CHECK(json_valid(request_json)),
      created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
      updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    ) STRICT;
    CREATE INDEX management_requests_scope ON management_requests(protocol, profile_id);
  "#;

/// Migration 14: the draft version 13 vault table is replaced.
pub(super) const DRAFT_VAULT_SQL: &str = r#"
    ALTER TABLE password_vault RENAME TO password_vault_draft_v13;

    CREATE TABLE password_vault (
      singleton INTEGER PRIMARY KEY CHECK(singleton = 1),
      format_version INTEGER NOT NULL CHECK(format_version IN (2, 3)),
      vault_id TEXT NOT NULL UNIQUE CHECK(length(vault_id) BETWEEN 16 AND 64),
      unlock_policy TEXT NOT NULL
        CHECK(unlock_policy IN ('never', 'startup', 'credential')),
      kdf_algorithm TEXT NOT NULL CHECK(kdf_algorithm = 'scrypt'),
      kdf_salt BLOB NOT NULL CHECK(length(kdf_salt) = 16),
      kdf_cost INTEGER NOT NULL CHECK(kdf_cost BETWEEN 1024 AND 1048576),
      kdf_block_size INTEGER NOT NULL CHECK(kdf_block_size BETWEEN 1 AND 32),
      kdf_parallelism INTEGER NOT NULL CHECK(kdf_parallelism BETWEEN 1 AND 16),
      master_key_nonce BLOB NOT NULL CHECK(length(master_key_nonce) = 12),
      master_key_ciphertext BLOB NOT NULL CHECK(length(master_key_ciphertext) = 32),
      master_key_tag BLOB NOT NULL CHECK(length(master_key_tag) = 16),
      created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
      updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
    ) STRICT;
  "#;

/// Migration 14: the draft vault row is copied over with a new vault id.
pub(super) const DRAFT_VAULT_COPY_SQL: &str = r#"
    INSERT INTO password_vault(
      singleton, format_version, vault_id, unlock_policy,
      kdf_algorithm, kdf_salt, kdf_cost, kdf_block_size,
      kdf_parallelism, master_key_nonce, master_key_ciphertext,
      master_key_tag, created_at, updated_at
    )
    SELECT
      singleton, format_version, ?, 'startup',
      kdf_algorithm, kdf_salt, kdf_cost, kdf_block_size,
      kdf_parallelism, master_key_nonce, master_key_ciphertext,
      master_key_tag, created_at, updated_at
    FROM password_vault_draft_v13
  "#;

/// Migration 15: the vault key check column.
pub(super) const KEY_CHECK_SQL: &str = r#"
    ALTER TABLE password_vault
      ADD COLUMN key_check BLOB
      CHECK(key_check IS NULL OR length(key_check) = 32);
  "#;

/// The stored connection_profiles definition.
pub(super) const REBUILD_TABLE_SQL: &str =
    r#"SELECT sql FROM sqlite_schema WHERE type = 'table' AND name = 'connection_profiles'"#;

/// The stored connection_profiles index definitions.
pub(super) const REBUILD_INDEXES_SQL: &str = r#"
      SELECT sql FROM sqlite_schema
      WHERE type = 'index' AND tbl_name = 'connection_profiles' AND sql IS NOT NULL
    "#;

/// Rebuild: statements before the rebuilt CREATE TABLE.
pub(super) const REBUILD_BEFORE_SQL: &str = r#"
    CREATE TEMP TABLE connection_tags_rebuild AS SELECT * FROM connection_tags;
    "#;

/// Rebuild: statements after the rebuilt CREATE TABLE.
pub(super) const REBUILD_AFTER_SQL: &str = r#";
    INSERT INTO connection_profiles_rebuilt SELECT * FROM connection_profiles;
    DROP TABLE connection_profiles;
    ALTER TABLE connection_profiles_rebuilt RENAME TO connection_profiles;
  "#;

/// Rebuild: put the connection tags back.
pub(super) const REBUILD_RESTORE_TAGS_SQL: &str = r#"
    INSERT INTO connection_tags SELECT * FROM connection_tags_rebuild;
    DROP TABLE connection_tags_rebuild;
  "#;
