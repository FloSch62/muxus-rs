//! Raw ssh_config option rows: the legacy-device algorithm preset and the
//! algorithm entries the SSH engine cannot offer
//! (`client/src/components/host-editor/advanced-options.ts`).

use std::collections::{BTreeMap, HashSet};

use muxus_core::api::HostOptionExtra;
use muxus_core::js::js_trim;

/// The algorithm lines an old console server or switch typically needs —
/// append forms, so modern hosts matched by the same block keep working.
pub const LEGACY_ALGORITHM_PRESET: [(&str, &str); 3] = [
    (
        "KexAlgorithms",
        "+diffie-hellman-group-exchange-sha1,diffie-hellman-group14-sha1,diffie-hellman-group1-sha1",
    ),
    ("HostKeyAlgorithms", "+ssh-rsa"),
    ("Ciphers", "+aes256-cbc,aes192-cbc,aes128-cbc,3des-cbc"),
];

/// The preset as option rows.
pub fn legacy_preset_rows() -> Vec<HostOptionExtra> {
    LEGACY_ALGORITHM_PRESET
        .iter()
        .map(|(keyword, value)| HostOptionExtra {
            keyword: (*keyword).to_owned(),
            value: (*value).to_owned(),
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LegacyPresetState {
    Missing,
    Partial,
    Enabled,
    Conflict,
}

fn find_row<'a>(extras: &'a [HostOptionExtra], keyword: &str) -> Option<&'a HostOptionExtra> {
    extras
        .iter()
        .find(|extra| js_trim(&extra.keyword).to_lowercase() == keyword.to_lowercase())
}

/// Whether the raw algorithm rows already provide the legacy-device preset.
pub fn legacy_preset_state(extras: &[HostOptionExtra]) -> LegacyPresetState {
    let mut configured = 0;
    let mut required = 0;
    let mut custom_policy = false;
    let mut conflict = false;

    for (keyword, value) in LEGACY_ALGORITHM_PRESET {
        let required_names = algorithm_names(value);
        required += required_names.len();
        let Some(row) = find_row(extras, keyword) else {
            continue;
        };
        custom_policy = true;
        if js_trim(&row.value).starts_with('-') {
            conflict = true;
            continue;
        }
        let current: HashSet<String> = algorithm_names(&row.value).into_iter().collect();
        configured += required_names
            .iter()
            .filter(|name| current.contains(*name))
            .count();
    }

    if configured == required {
        LegacyPresetState::Enabled
    } else if conflict {
        LegacyPresetState::Conflict
    } else if configured > 0 || custom_policy {
        LegacyPresetState::Partial
    } else {
        LegacyPresetState::Missing
    }
}

/// Add the legacy names without replacing a user's existing exact, append, or
/// prepend policy. Removal policies are left untouched because rewriting one
/// would silently discard the user's exclusions.
pub fn apply_legacy_preset(extras: &[HostOptionExtra]) -> Vec<HostOptionExtra> {
    let mut next = extras.to_vec();
    for (keyword, preset_value) in LEGACY_ALGORITHM_PRESET {
        let index = next
            .iter()
            .position(|extra| js_trim(&extra.keyword).to_lowercase() == keyword.to_lowercase());
        let Some(index) = index else {
            next.push(HostOptionExtra {
                keyword: keyword.to_owned(),
                value: preset_value.to_owned(),
            });
            continue;
        };
        let value = js_trim(&next[index].value).to_owned();
        if value.starts_with('-') {
            continue;
        }
        let prefix = match value.chars().next() {
            Some(c @ ('+' | '^')) => c.to_string(),
            _ => String::new(),
        };
        let current_names = algorithm_names(&value);
        let seen: HashSet<&String> = current_names.iter().collect();
        let missing: Vec<String> = algorithm_names(preset_value)
            .into_iter()
            .filter(|name| !seen.contains(name))
            .collect();
        if missing.is_empty() {
            continue;
        }
        next[index].value = if current_names.is_empty() {
            preset_value.to_owned()
        } else {
            let mut names = current_names.clone();
            names.extend(missing);
            format!("{prefix}{}", names.join(","))
        };
    }
    next
}

/// Entries of an algorithm-list option the dialer would have to skip: not
/// `*`/`?` patterns, and absent from the SSH engine's supported table for the
/// keyword. Non-algorithm keywords and a missing table return nothing.
pub fn unsupported_entries(
    keyword: &str,
    value: &str,
    tables: Option<&BTreeMap<String, Vec<String>>>,
) -> Vec<String> {
    let Some(tables) = tables else {
        return Vec::new();
    };
    let wanted = js_trim(keyword).to_lowercase();
    let Some((_, supported)) = tables.iter().find(|(k, _)| k.to_lowercase() == wanted) else {
        return Vec::new();
    };
    strip_policy_prefix(value)
        .split(',')
        .map(|entry| js_trim(entry).to_lowercase())
        .filter(|entry| !entry.is_empty())
        .filter(|entry| !entry.contains(['*', '?']) && !supported.contains(entry))
        .collect()
}

/// `value.replace(/^[+^-]/, '')`.
fn strip_policy_prefix(value: &str) -> &str {
    value.strip_prefix(['+', '^', '-']).unwrap_or(value)
}

fn algorithm_names(value: &str) -> Vec<String> {
    strip_policy_prefix(js_trim(value))
        .split(',')
        .map(|entry| js_trim(entry).to_lowercase())
        .filter(|entry| !entry.is_empty())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use muxus_core::api::DIAL_TIME_KEYWORDS;

    fn row(keyword: &str, value: &str) -> HostOptionExtra {
        HostOptionExtra {
            keyword: keyword.into(),
            value: value.into(),
        }
    }

    fn tables() -> BTreeMap<String, Vec<String>> {
        BTreeMap::from([
            (
                "Ciphers".to_owned(),
                vec![
                    "aes128-cbc".to_owned(),
                    "aes256-ctr".to_owned(),
                    "chacha20-poly1305@openssh.com".to_owned(),
                ],
            ),
            (
                "KexAlgorithms".to_owned(),
                vec![
                    "curve25519-sha256".to_owned(),
                    "diffie-hellman-group14-sha1".to_owned(),
                ],
            ),
        ])
    }

    #[test]
    fn flags_entries_the_ssh_engine_cannot_offer_keeping_prefix_and_case_out_of_it() {
        let tables = tables();
        assert_eq!(
            unsupported_entries("ciphers", "+AES128-CBC,serpent-cbc", Some(&tables)),
            vec!["serpent-cbc"]
        );
        assert_eq!(
            unsupported_entries(
                "KexAlgorithms",
                "sntrup761x25519-sha512@openssh.com",
                Some(&tables)
            ),
            vec!["sntrup761x25519-sha512@openssh.com"]
        );
    }

    #[test]
    fn skips_wildcard_patterns_unknown_keywords_and_missing_tables() {
        let tables = tables();
        assert!(unsupported_entries("Ciphers", "-*cbc", Some(&tables)).is_empty());
        assert!(unsupported_entries("Compression", "yes", Some(&tables)).is_empty());
        assert!(unsupported_entries("Ciphers", "serpent-cbc", None).is_empty());
    }

    #[test]
    fn only_uses_append_forms_and_dial_time_keywords() {
        for (keyword, value) in LEGACY_ALGORITHM_PRESET {
            assert!(value.starts_with('+'));
            assert!(DIAL_TIME_KEYWORDS.contains(&keyword.to_lowercase().as_str()));
        }
    }

    #[test]
    fn adds_the_preset_to_a_host_using_modern_defaults() {
        let extras = vec![row("Compression", "yes")];
        let next = apply_legacy_preset(&extras);
        assert_eq!(legacy_preset_state(&extras), LegacyPresetState::Missing);
        assert_eq!(legacy_preset_state(&next), LegacyPresetState::Enabled);
        assert_eq!(next[0], extras[0]);
        assert_eq!(next[1..], legacy_preset_rows()[..]);
    }

    #[test]
    fn completes_existing_append_and_exact_policies_without_replacing_them() {
        let extras = vec![
            row("KexAlgorithms", "+curve25519-sha256"),
            row("HostKeyAlgorithms", "ssh-ed25519"),
        ];
        let next = apply_legacy_preset(&extras);
        assert_eq!(legacy_preset_state(&extras), LegacyPresetState::Partial);
        assert!(next[0].value.contains("+curve25519-sha256,"));
        assert!(next[0].value.contains("diffie-hellman-group14-sha1"));
        assert!(next[0].value.ends_with(",diffie-hellman-group1-sha1"));
        assert_eq!(next[1].value, "ssh-ed25519,ssh-rsa");
        assert_eq!(legacy_preset_state(&next), LegacyPresetState::Enabled);
    }

    #[test]
    fn leaves_removal_policies_untouched_for_manual_review() {
        let extras = vec![row("Ciphers", "-*cbc")];
        assert_eq!(legacy_preset_state(&extras), LegacyPresetState::Conflict);
        assert_eq!(
            apply_legacy_preset(&extras)
                .iter()
                .find(|r| r.keyword == "Ciphers"),
            Some(&extras[0])
        );
    }
}
