//! `client-state.json`: the desktop app's key/value store for UI state
//! (`loadClientState`/`saveClientState` in `electron/src/main.ts`, served to
//! the renderer as zustand's `StateStorage` by `persist-storage.ts`).
//!
//! The file is one JSON object of string values. Each value is itself JSON
//! text, such as the `muxus-prefs` envelope. Keys this build does not know
//! are kept as they are, so both builds can share the file.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use indexmap::IndexMap;

/// The zustand key the preferences live under.
pub const PREFS_STORAGE_NAME: &str = "muxus-prefs";
/// The release the user chose not to hear about again (plain version text).
pub const DISMISSED_UPDATE_KEY: &str = "muxus-dismissed-update-version";
/// The desktop update the user dismissed.
pub const DISMISSED_DESKTOP_UPDATE_KEY: &str = "muxus-dismissed-desktop-update";
/// When the last settings backup was exported (ISO timestamp).
pub const LAST_BACKUP_KEY: &str = "muxus-last-backup-at";

/// The parsed file plus where it lives. Insertion order is the file's order,
/// as a JavaScript object keeps it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientStateFile {
    path: PathBuf,
    items: IndexMap<String, String>,
}

impl ClientStateFile {
    /// Read the file. Anything unreadable, or anything but a JSON object,
    /// reads as empty; values that are not strings are dropped.
    pub fn load(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let items = fs::read_to_string(&path)
            .map(|text| parse_client_state(&text))
            .unwrap_or_default();
        Self { path, items }
    }

    /// An empty store that will be written to `path`.
    pub fn empty(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            items: IndexMap::new(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// `StateStorage.getItem`.
    pub fn get_item(&self, name: &str) -> Option<&str> {
        self.items.get(name).map(String::as_str)
    }

    /// `StateStorage.setItem`: an existing key keeps its place, a new one is
    /// appended (`{ ...state, [name]: value }`).
    pub fn set_item(&mut self, name: impl Into<String>, value: impl Into<String>) {
        self.items.insert(name.into(), value.into());
    }

    /// `StateStorage.removeItem` (`delete next[name]`); later keys keep their order.
    pub fn remove_item(&mut self, name: &str) -> Option<String> {
        self.items.shift_remove(name)
    }

    /// Every item, in file order.
    pub fn items(&self) -> &IndexMap<String, String> {
        &self.items
    }

    /// Write the file the way `saveClientState` does: pretty JSON with two
    /// spaces and a trailing newline, through a temporary file renamed into
    /// place, readable by the user only.
    pub fn save(&self) -> io::Result<()> {
        write_client_state(&self.path, &self.items)
    }

    /// Read the file again (another process may have written it).
    pub fn reload(&mut self) {
        self.items = fs::read_to_string(&self.path)
            .map(|text| parse_client_state(&text))
            .unwrap_or_default();
    }
}

/// `loadClientState`'s parsing: a JSON object's string values, in order.
pub fn parse_client_state(text: &str) -> IndexMap<String, String> {
    match serde_json::from_str::<serde_json::Value>(text) {
        Ok(serde_json::Value::Object(map)) => map
            .into_iter()
            .filter_map(|(key, value)| match value {
                serde_json::Value::String(value) => Some((key, value)),
                _ => None,
            })
            .collect(),
        _ => IndexMap::new(),
    }
}

/// `JSON.stringify(state, null, 2) + '\n'`.
pub fn format_client_state(items: &IndexMap<String, String>) -> String {
    let mut text = serde_json::to_string_pretty(items).expect("string maps always serialize");
    text.push('\n');
    text
}

/// `saveClientState`: create the folder, write `<file>.tmp` with mode 0600,
/// then rename it over the file.
pub fn write_client_state(path: &Path, items: &IndexMap<String, String>) -> io::Result<()> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        fs::create_dir_all(parent)?;
    }
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    write_private(&tmp, format_client_state(items).as_bytes())?;
    fs::rename(&tmp, path)
}

/// Write a file created with mode 0600 (the mode only applies on creation,
/// like Node's `writeFileSync(file, data, { mode })`).
pub(crate) fn write_private(path: &Path, data: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    file.write_all(data)?;
    file.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_only_string_values_of_an_object() {
        let items = parse_client_state(r#"{"b":"2","a":"1","n":3,"o":{"x":1},"z":"last"}"#);
        assert_eq!(items.keys().collect::<Vec<_>>(), vec!["b", "a", "z"]);
        assert!(parse_client_state("[\"a\"]").is_empty());
        assert!(parse_client_state("null").is_empty());
        assert!(parse_client_state("{not json").is_empty());
    }

    #[test]
    fn formats_like_json_stringify_with_two_spaces() {
        let mut items = IndexMap::new();
        assert_eq!(format_client_state(&items), "{}\n");
        items.insert(
            "muxus-prefs".to_owned(),
            "{\"state\":{},\"version\":17}".to_owned(),
        );
        items.insert(
            "muxus-last-backup-at".to_owned(),
            "2026-01-01T00:00:00.000Z".to_owned(),
        );
        assert_eq!(
            format_client_state(&items),
            "{\n  \"muxus-prefs\": \"{\\\"state\\\":{},\\\"version\\\":17}\",\n  \"muxus-last-backup-at\": \"2026-01-01T00:00:00.000Z\"\n}\n"
        );
    }

    #[test]
    fn keeps_positions_on_update_and_order_on_remove() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("client-state.json");
        let mut file = ClientStateFile::load(&path);
        assert!(file.items().is_empty());
        file.set_item("a", "1");
        file.set_item("b", "2");
        file.set_item("c", "3");
        file.set_item("a", "4");
        file.remove_item("b");
        file.save().unwrap();
        let again = ClientStateFile::load(&path);
        assert_eq!(
            again.items().iter().collect::<Vec<_>>(),
            vec![
                (&"a".to_owned(), &"4".to_owned()),
                (&"c".to_owned(), &"3".to_owned())
            ]
        );
        assert!(!path.with_extension("json.tmp").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
    }
}
