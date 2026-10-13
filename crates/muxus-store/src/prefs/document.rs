//! The stored preferences as zustand's `persist` middleware keeps them: the
//! `muxus-prefs` item holds `JSON.stringify({ state, version })`.
//!
//! The document keeps the stored JSON itself, so values this build reads but
//! does not change are written back exactly as they were. Rehydration follows
//! zustand: a snapshot whose numeric version differs from
//! [`PREFS_VERSION`] — older *or newer* — goes through
//! [`migrate_prefs_state`](super::migrate::migrate_prefs_state) and is then
//! written back at this version; a snapshot at this version, or without a
//! numeric version, is merged as it is.

use serde_json::{Map, Value};

use super::builtin_highlight_profiles::{builtin_highlight_profiles_json, is_truthy};
use super::migrate::{merge_persisted_prefs, migrate_prefs_state};
use super::state::PrefsState;

/// The preferences store's persist version.
pub const PREFS_VERSION: u32 = 17;

/// The default state object, as the TypeScript store writes it.
pub fn default_state_map() -> Map<String, Value> {
    let mut map = PrefsState::default().to_json_map();
    // Built-in profiles keep the key order the TypeScript seeds them with.
    map.insert(
        "keywordHighlightProfiles".into(),
        Value::Array(builtin_highlight_profiles_json().to_vec()),
    );
    map
}

/// The `state` object of the `muxus-prefs` item.
#[derive(Debug, Clone, PartialEq)]
pub struct PrefsDocument {
    state: Map<String, Value>,
}

impl Default for PrefsDocument {
    fn default() -> Self {
        Self {
            state: default_state_map(),
        }
    }
}

/// What rehydrating a stored item produced.
#[derive(Debug, Clone, PartialEq)]
pub struct Rehydration {
    pub document: PrefsDocument,
    /// The snapshot was migrated; zustand writes the result back right away.
    pub migrated: bool,
    /// The stored text was not JSON; the defaults stay and nothing is written.
    pub error: Option<String>,
}

impl PrefsDocument {
    /// A document over a state object (for tests and imports).
    pub fn from_state_map(state: Map<String, Value>) -> Self {
        Self { state }
    }

    /// Rehydrate from the stored `muxus-prefs` text, or the defaults when
    /// there is none.
    pub fn rehydrate(stored: Option<&str>) -> Rehydration {
        let defaults = default_state_map();
        let fresh = |error: Option<String>| Rehydration {
            document: PrefsDocument {
                state: defaults.clone(),
            },
            migrated: false,
            error,
        };
        let Some(text) = stored else {
            return fresh(None);
        };
        let parsed: Value = match serde_json::from_str(text) {
            Ok(parsed) => parsed,
            Err(error) => return fresh(Some(error.to_string())),
        };
        if !is_truthy(&parsed) {
            return fresh(None);
        }
        let state = parsed.get("state").cloned().unwrap_or(Value::Null);
        match parsed.get("version").and_then(Value::as_f64) {
            Some(version) if version != f64::from(PREFS_VERSION) => Rehydration {
                document: PrefsDocument {
                    state: merge_persisted_prefs(&migrate_prefs_state(&state, version), &defaults),
                },
                migrated: true,
                error: None,
            },
            _ => Rehydration {
                document: PrefsDocument {
                    state: merge_persisted_prefs(&state, &defaults),
                },
                migrated: false,
                error: None,
            },
        }
    }

    /// The stored state object, in its stored order.
    pub fn state_map(&self) -> &Map<String, Value> {
        &self.state
    }

    /// The typed preferences.
    pub fn prefs(&self) -> PrefsState {
        PrefsState::from_json_map(&self.state)
    }

    /// Take a changed preferences value. Only fields whose value differs from
    /// what the document reads as are rewritten; everything else keeps its
    /// stored JSON. Returns whether anything changed.
    pub fn update(&mut self, next: &PrefsState) -> bool {
        let before = self.prefs().to_json_map();
        let after = next.to_json_map();
        let mut changed = false;
        for (key, value) in &after {
            if before.get(key) != Some(value) {
                // An existing key keeps its place, a new one is appended.
                self.state.insert(key.clone(), value.clone());
                changed = true;
            }
        }
        for key in before.keys().filter(|key| !after.contains_key(*key)) {
            if self.state.shift_remove(key).is_some() {
                changed = true;
            }
        }
        changed
    }

    /// `JSON.stringify({ state, version })`, the text stored under `muxus-prefs`.
    pub fn to_envelope_json(&self) -> String {
        let mut envelope = Map::new();
        envelope.insert("state".into(), Value::Object(self.state.clone()));
        envelope.insert("version".into(), Value::from(PREFS_VERSION));
        Value::Object(envelope).to_string()
    }
}
