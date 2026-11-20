//! KeePass databases, read from the file (`keepass.ts`).

use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::SystemTime;

use async_trait::async_trait;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use keepass::db::{EntryRef, GroupRef};
use keepass::error::{
    CryptographyError, DatabaseFormatError, DatabaseOpenError, DatabaseVersionParseError,
    Kdbx3OpenError, Kdbx3OuterHeaderError, Kdbx4InnerHeaderError, Kdbx4OpenError,
    Kdbx4OuterHeaderError,
};
use keepass::{Database, DatabaseKey};
use muxus_core::api::{
    PasswordManagerConfig, PasswordManagerEntry, PasswordManagerKind, PasswordManagerState,
};
use regex::{Captures, Regex};

use crate::cli::basename;
use crate::host::HostEnv;
use crate::provider::{
    PasswordManagerError, PasswordManagerProblem, PasswordManagerProvider, PasswordManagerResult,
    ProviderStatus, non_empty, sort_entries, trimmed,
};

/// What opens the database: its password, its key file, or both.
#[derive(Clone)]
struct Credentials {
    /// `None` for a database protected by a key file alone.
    password: Option<String>,
    key_file: Option<Vec<u8>>,
}

impl Credentials {
    fn key(&self) -> DatabaseKey {
        let mut key = DatabaseKey::new();
        if let Some(password) = &self.password {
            key = key.with_password(password);
        }
        if let Some(key_file) = &self.key_file {
            // Reading from memory cannot fail.
            key = key
                .clone()
                .with_keyfile(&mut key_file.as_slice())
                .unwrap_or(key);
        }
        key
    }
}

struct OpenDatabase {
    db: Database,
    credentials: Credentials,
    /// The file as it was when read, to notice a save from KeePass.
    modified: Option<SystemTime>,
    size: u64,
}

/// Reads a KeePass database file directly, so it works with KeePass, KeePassXC and every
/// other program that writes .kdbx, whether or not one is running. The database is opened
/// with its own password (and key file) and kept open in memory until it is locked or
/// Muxus quits; nothing is ever written back.
pub struct KeePassProvider {
    file: String,
    key_file: Option<String>,
    open: Mutex<Option<Arc<OpenDatabase>>>,
}

impl KeePassProvider {
    pub fn new(config: &PasswordManagerConfig, host: &HostEnv) -> Self {
        Self {
            file: host.expand_home(&trimmed(config.path.as_deref()).unwrap_or_default()),
            key_file: trimmed(config.key_file.as_deref()).map(|file| host.expand_home(&file)),
            open: Mutex::new(None),
        }
    }

    fn opened(&self) -> Option<Arc<OpenDatabase>> {
        self.open
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    fn set_open(&self, open: Option<Arc<OpenDatabase>>) {
        *self.open.lock().unwrap_or_else(PoisonError::into_inner) = open;
    }

    /// The open database, read again when KeePass has saved the file since.
    async fn database(&self) -> PasswordManagerResult<Arc<OpenDatabase>> {
        let Some(open) = self.opened() else {
            return Err(PasswordManagerError::locked(
                "The KeePass database is locked.",
            ));
        };
        let Ok(info) = tokio::fs::metadata(&self.file).await else {
            return Err(PasswordManagerError::unavailable(format!(
                "The database {} was not found.",
                self.file
            )));
        };
        if info.modified().ok() == open.modified && info.len() == open.size {
            return Ok(open);
        }
        match self.load(open.credentials.clone()).await {
            Ok(reloaded) => {
                let reloaded = Arc::new(reloaded);
                self.set_open(Some(reloaded.clone()));
                Ok(reloaded)
            }
            // Its password was changed in KeePass: the remembered one no longer opens it.
            Err(error) if error.problem == PasswordManagerProblem::InvalidUnlock => {
                self.set_open(None);
                Err(PasswordManagerError::locked(
                    "The KeePass database password has changed.",
                ))
            }
            Err(error) => Err(error),
        }
    }

    async fn load(&self, credentials: Credentials) -> PasswordManagerResult<OpenDatabase> {
        let unreadable = || {
            PasswordManagerError::unavailable(format!(
                "The database {} could not be read.",
                self.file
            ))
        };
        let info = tokio::fs::metadata(&self.file)
            .await
            .map_err(|_| unreadable())?;
        let data = tokio::fs::read(&self.file)
            .await
            .map_err(|_| unreadable())?;
        let name = basename(&self.file).to_owned();
        let has_key_file = self.key_file.is_some();
        // Deriving the key is made to take a while; keep it off the async threads.
        let key = credentials.key();
        let parsed = tokio::task::spawn_blocking(move || Database::parse(&data, key))
            .await
            .map_err(|_| PasswordManagerError::failed(format!("{name} could not be opened.")))?;
        let name = basename(&self.file);
        let db = parsed.map_err(|error| match open_failure(&error) {
            OpenFailure::WrongKey => PasswordManagerError::invalid_unlock(if has_key_file {
                "The database password or key file is wrong."
            } else {
                "The database password is wrong."
            }),
            OpenFailure::NotADatabase => {
                PasswordManagerError::failed(format!("{name} is not a KeePass database."))
            }
            OpenFailure::Unsupported => PasswordManagerError::failed(format!(
                "{name} uses a database format or cipher Muxus cannot read."
            )),
            OpenFailure::Other => {
                PasswordManagerError::failed(format!("{name} could not be opened."))
            }
        })?;
        Ok(OpenDatabase {
            db,
            credentials,
            modified: info.modified().ok(),
            size: info.len(),
        })
    }
}

/// Why a database did not open, as far as the person can do something about it.
enum OpenFailure {
    WrongKey,
    NotADatabase,
    /// A format version, cipher, compression or key derivation this reader does not have.
    Unsupported,
    Other,
}

fn open_failure(error: &DatabaseOpenError) -> OpenFailure {
    match error {
        DatabaseOpenError::Key(_) => OpenFailure::WrongKey,
        // KDBX 3 has no check of the key ahead of the payload: a wrong one decrypts to
        // bytes whose padding does not add up.
        DatabaseOpenError::Cryptography(CryptographyError::InvalidPadding(_)) => {
            OpenFailure::WrongKey
        }
        DatabaseOpenError::VersionParse(DatabaseVersionParseError::InvalidKDBXIdentifier) => {
            OpenFailure::NotADatabase
        }
        DatabaseOpenError::VersionParse(DatabaseVersionParseError::InvalidKDBXVersion {
            ..
        })
        | DatabaseOpenError::UnsupportedVersion => OpenFailure::Unsupported,
        DatabaseOpenError::Format(DatabaseFormatError::Kdbx3(Kdbx3OpenError::OuterHeader(
            Kdbx3OuterHeaderError::InnerCipher(_)
            | Kdbx3OuterHeaderError::OuterCipher(_)
            | Kdbx3OuterHeaderError::Compression(_),
        )))
        | DatabaseOpenError::Format(DatabaseFormatError::Kdbx4(
            Kdbx4OpenError::OuterHeader(
                Kdbx4OuterHeaderError::OuterCipherConfig(_)
                | Kdbx4OuterHeaderError::CompressionConfig(_)
                | Kdbx4OuterHeaderError::KdfConfig(_),
            )
            | Kdbx4OpenError::InnerHeader(Kdbx4InnerHeaderError::InnerCipherConfig(_)),
        )) => OpenFailure::Unsupported,
        _ => OpenFailure::Other,
    }
}

#[async_trait]
impl PasswordManagerProvider for KeePassProvider {
    fn kind(&self) -> PasswordManagerKind {
        PasswordManagerKind::Keepass
    }

    async fn status(&self) -> PasswordManagerResult<ProviderStatus> {
        if self.file.is_empty() {
            return Ok(ProviderStatus::new(
                PasswordManagerState::Unavailable,
                "No database file is set.",
            ));
        }
        if tokio::fs::metadata(&self.file).await.is_err() {
            return Ok(ProviderStatus::new(
                PasswordManagerState::Unavailable,
                format!("The database {} was not found.", self.file),
            ));
        }
        Ok(if self.opened().is_some() {
            ProviderStatus::ready()
        } else {
            ProviderStatus::new(
                PasswordManagerState::Locked,
                "Enter the database password to unlock it.",
            )
        })
    }

    fn can_unlock(&self) -> bool {
        true
    }

    async fn unlock(&self, password: &str) -> PasswordManagerResult<()> {
        let key_file = match &self.key_file {
            Some(file) => Some(tokio::fs::read(file).await.map_err(|_| {
                PasswordManagerError::unavailable(format!("The key file {file} could not be read."))
            })?),
            None => None,
        };
        // A database protected by a key file alone has no password component.
        let password = (!(password.is_empty() && key_file.is_some())).then(|| password.to_owned());
        let open = self.load(Credentials { password, key_file }).await?;
        self.set_open(Some(Arc::new(open)));
        Ok(())
    }

    async fn lock(&self) -> PasswordManagerResult<()> {
        self.set_open(None);
        Ok(())
    }

    fn dispose(&self) {
        self.set_open(None);
    }

    async fn entries(&self) -> PasswordManagerResult<Vec<PasswordManagerEntry>> {
        let open = self.database().await?;
        let db = &open.db;
        let root = db.root().id();
        let mut entries = Vec::new();
        for entry in live_entries(db) {
            // An entry without a password has nothing to log in with.
            if field_text(&entry, "Password").is_empty() {
                continue;
            }
            let username = non_empty(Some(&resolved_field(db, &entry, "UserName", 0)));
            let url = non_empty(Some(field_text(&entry, "URL")));
            let group = non_empty(Some(&group_path(&entry, root)));
            let title = non_empty(Some(field_text(&entry, "Title")))
                .or_else(|| username.clone())
                .or_else(|| url.clone())
                .unwrap_or_else(|| "Untitled entry".to_owned());
            entries.push(PasswordManagerEntry {
                id: entry_id(&entry),
                title,
                username,
                url,
                group,
            });
        }
        Ok(sort_entries(entries))
    }

    async fn password(&self, entry_id: &str) -> PasswordManagerResult<String> {
        let open = self.database().await?;
        let db = &open.db;
        let entry = live_entries(db)
            .into_iter()
            .find(|candidate| self::entry_id(candidate) == entry_id)
            .ok_or_else(|| {
                PasswordManagerError::entry_missing(
                    "The entry is no longer in the KeePass database.",
                )
            })?;
        Ok(resolved_field(db, &entry, "Password", 0))
    }
}

/// An entry's UUID as KeePass files and kdbxweb write it: its 16 bytes in base64.
fn entry_id(entry: &EntryRef<'_>) -> String {
    STANDARD.encode(entry.id().uuid().as_bytes())
}

/// Every entry outside the recycle bin.
fn live_entries(db: &Database) -> Vec<EntryRef<'_>> {
    let bin = db
        .meta
        .recyclebin_enabled
        .unwrap_or(false)
        .then_some(db.meta.recyclebin_uuid)
        .flatten();
    let mut ids = Vec::new();
    collect_entries(&db.root(), bin.as_ref().map(|bin| bin.as_bytes()), &mut ids);
    ids.into_iter().filter_map(|id| db.entry(id)).collect()
}

fn collect_entries(
    group: &GroupRef<'_>,
    bin: Option<&[u8; 16]>,
    ids: &mut Vec<keepass::db::EntryId>,
) {
    if bin.is_some_and(|bin| group.id().uuid().as_bytes() == bin) {
        return;
    }
    ids.extend(group.entry_ids());
    for child in group.groups() {
        collect_entries(&child, bin, ids);
    }
}

fn field_text<'a>(entry: &'a EntryRef<'_>, name: &str) -> &'a str {
    entry.get(name).unwrap_or("")
}

static FIELD_REFERENCE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\{REF:([PU])@I:([0-9A-Fa-f]{32})\}").expect("a valid pattern"));

/// A field's value with KeePass field references followed: `{REF:P@I:<uuid>}` stands for
/// the password, and `{REF:U@I:<uuid>}` for the user name, of the entry with that UUID.
/// Other placeholders are left as written.
fn resolved_field(db: &Database, entry: &EntryRef<'_>, name: &str, depth: usize) -> String {
    let text = field_text(entry, name);
    if depth > 4 {
        return text.to_owned();
    }
    FIELD_REFERENCE
        .replace_all(text, |found: &Captures<'_>| {
            let id = hex::decode(&found[2])
                .map(|bytes| STANDARD.encode(bytes))
                .unwrap_or_default();
            let target = live_entries(db)
                .into_iter()
                .find(|candidate| entry_id(candidate) == id);
            match target {
                Some(target) => {
                    let field = if &found[1] == "P" {
                        "Password"
                    } else {
                        "UserName"
                    };
                    resolved_field(db, &target, field, depth + 1)
                }
                None => found[0].to_owned(),
            }
        })
        .into_owned()
}

/// "Servers / Lab", leaving out the database's root group.
fn group_path(entry: &EntryRef<'_>, root: keepass::db::GroupId) -> String {
    let db = entry.database();
    let mut names = Vec::new();
    let mut current = Some(entry.parent().id());
    while let Some(group) = current.filter(|id| *id != root).and_then(|id| db.group(id)) {
        if !group.name.is_empty() {
            names.push(group.name.clone());
        }
        current = group.parent().map(|parent| parent.id());
    }
    names.reverse();
    names.join(" / ")
}

/// Whether a path names a file that ends like a KeePass database, for the default name.
pub(crate) fn database_name(path: &str) -> Option<String> {
    let file = path.rsplit(['/', '\\']).next()?;
    let name = match file.len().checked_sub(5) {
        Some(at) if file.is_char_boundary(at) && file[at..].eq_ignore_ascii_case(".kdbx") => {
            &file[..at]
        }
        _ => file,
    };
    (!name.is_empty()).then(|| name.to_owned())
}
