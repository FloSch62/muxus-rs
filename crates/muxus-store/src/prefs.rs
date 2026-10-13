//! The desktop app's preferences and other UI state, stored the way the
//! Electron build stores them.
//!
//! Everything lives in `client-state.json` in the app's data folder (see
//! [`crate::paths::AppPaths::client_state_file`]): a JSON object of string
//! items, the way the renderer's zustand storage sees it. The preferences are
//! the `muxus-prefs` item, `{"state":{…},"version":17}`. [`PrefsStore`] reads
//! and writes them, running the same migrations as the TypeScript store, so
//! either build can open what the other wrote.
//!
//! The submodules hold the pure helpers the preferences depend on: command
//! buttons, highlighting profiles (with the built-in ones), custom terminal
//! schemes, layout bounds and window placement.

pub mod builtin_highlight_profiles;
pub mod client_state;
pub mod command_buttons;
pub mod custom_schemes;
pub mod document;
pub mod highlight_profiles;
pub mod layout;
pub mod migrate;
pub mod state;
pub mod window_state;

use std::io;
use std::path::PathBuf;

pub use client_state::{
    ClientStateFile, DISMISSED_DESKTOP_UPDATE_KEY, DISMISSED_UPDATE_KEY, LAST_BACKUP_KEY,
    PREFS_STORAGE_NAME,
};
pub use document::{PREFS_VERSION, PrefsDocument, Rehydration, default_state_map};
pub use migrate::{merge_persisted_prefs, migrate_prefs_state};
pub use state::PrefsState;

/// The preferences plus the file they live in.
#[derive(Debug, Clone)]
pub struct PrefsStore {
    file: ClientStateFile,
    document: PrefsDocument,
}

impl PrefsStore {
    /// Read `client-state.json` and rehydrate the preferences. A snapshot
    /// that needed migrating is written back at once, as zustand does; that
    /// write is best effort (its error is returned beside the store).
    pub fn open(path: impl Into<PathBuf>) -> (Self, Option<io::Error>) {
        let file = ClientStateFile::load(path);
        let rehydration = PrefsDocument::rehydrate(file.get_item(PREFS_STORAGE_NAME));
        let mut store = Self {
            file,
            document: rehydration.document,
        };
        let error = if rehydration.migrated {
            store.save().err()
        } else {
            None
        };
        (store, error)
    }

    /// The typed preferences.
    pub fn prefs(&self) -> PrefsState {
        self.document.prefs()
    }

    /// The stored document (JSON in its stored order).
    pub fn document(&self) -> &PrefsDocument {
        &self.document
    }

    /// Take changed preferences and write them (the store's `set`, which
    /// persists on every change).
    pub fn set(&mut self, prefs: &PrefsState) -> io::Result<()> {
        self.document.update(prefs);
        self.save()
    }

    /// Change the preferences in memory only; [`PrefsStore::save`] writes them.
    pub fn update(&mut self, prefs: &PrefsState) -> bool {
        self.document.update(prefs)
    }

    /// Write the preferences item and the whole file.
    pub fn save(&mut self) -> io::Result<()> {
        self.file
            .set_item(PREFS_STORAGE_NAME, self.document.to_envelope_json());
        self.file.save()
    }

    /// Read the file again and take the preferences another window or the
    /// other build saved (`followPrefsFromOtherWindows`). Like a rehydrate,
    /// nothing is written unless the stored copy needed migrating.
    pub fn reload(&mut self) -> Option<io::Error> {
        self.file.reload();
        let rehydration = PrefsDocument::rehydrate(self.file.get_item(PREFS_STORAGE_NAME));
        self.document = rehydration.document;
        if rehydration.migrated {
            self.save().err()
        } else {
            None
        }
    }

    /// The other items of `client-state.json`.
    pub fn client_state(&self) -> &ClientStateFile {
        &self.file
    }

    /// The other items of `client-state.json`, for writing; call
    /// [`ClientStateFile::save`] afterwards.
    pub fn client_state_mut(&mut self) -> &mut ClientStateFile {
        &mut self.file
    }

    /// The release the user dismissed the update notice for.
    pub fn dismissed_update_version(&self) -> Option<&str> {
        self.file.get_item(DISMISSED_UPDATE_KEY)
    }

    pub fn set_dismissed_update_version(&mut self, version: &str) -> io::Result<()> {
        self.file.set_item(DISMISSED_UPDATE_KEY, version);
        self.file.save()
    }

    /// The desktop update the user dismissed.
    pub fn dismissed_desktop_update(&self) -> Option<&str> {
        self.file.get_item(DISMISSED_DESKTOP_UPDATE_KEY)
    }

    pub fn set_dismissed_desktop_update(&mut self, version: &str) -> io::Result<()> {
        self.file.set_item(DISMISSED_DESKTOP_UPDATE_KEY, version);
        self.file.save()
    }

    /// When the settings were last exported as a backup (ISO timestamp).
    pub fn last_backup_at(&self) -> Option<&str> {
        self.file.get_item(LAST_BACKUP_KEY)
    }

    pub fn set_last_backup_at(&mut self, created_at: &str) -> io::Result<()> {
        self.file.set_item(LAST_BACKUP_KEY, created_at);
        self.file.save()
    }
}
