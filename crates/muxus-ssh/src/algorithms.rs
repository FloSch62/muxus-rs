//! Translate the ssh_config algorithm lists (Ciphers, KexAlgorithms, HostKeyAlgorithms, MACs)
//! into the client's preferred-algorithm lists.
//!
//! OpenSSH list syntax: a leading `+` appends to the default set, `^` moves to its head, `-`
//! removes from it, and a bare list replaces it entirely. Entries may be `*`/`?` patterns; they
//! expand against the supported table.
//!
//! The config file is usually shared with OpenSSH, which implements algorithms the SSH engine
//! lacks (sntrup761x25519, ...). Such entries are dropped and reported instead of failing the
//! dial: an extra algorithm OpenSSH would offer must not break a host Muxus could otherwise
//! reach.
//!
//! The default and supported tables mirror what Muxus's TypeScript engine offered, so old network
//! gear that only speaks ECDH-NIST, CBC ciphers or SHA-1 MACs negotiates the same way: the
//! defaults keep ecdh-sha2-nistp*, the SHA-1 MACs and ssh-rsa, and the legacy kex
//! (diffie-hellman-group1-sha1, group14-sha1, gex-sha1), CBC/3DES ciphers and ssh-dss are
//! available on request.

use std::borrow::Cow;

use russh::keys::{Algorithm, EcdsaCurve, HashAlg};
use russh::{Preferred, cipher, compression, kex, mac};

use crate::config::glob_match;

/// Key exchange the client offers when the config says nothing. Post-quantum and Curve25519
/// first, then the ECDH and DH groups.
pub const DEFAULT_KEX: &[&str] = &[
    "mlkem768x25519-sha256",
    "curve25519-sha256",
    "curve25519-sha256@libssh.org",
    "ecdh-sha2-nistp256",
    "ecdh-sha2-nistp384",
    "ecdh-sha2-nistp521",
    "diffie-hellman-group-exchange-sha256",
    "diffie-hellman-group14-sha256",
    "diffie-hellman-group15-sha512",
    "diffie-hellman-group16-sha512",
    "diffie-hellman-group17-sha512",
    "diffie-hellman-group18-sha512",
];

/// Legacy key exchange available on request (old switches and console servers).
const LEGACY_KEX: &[&str] = &[
    "diffie-hellman-group-exchange-sha1",
    "diffie-hellman-group14-sha1", // REQUIRED
    "diffie-hellman-group1-sha1",  // REQUIRED
];

pub const DEFAULT_SERVER_HOST_KEY: &[&str] = &[
    "ssh-ed25519",
    "ecdsa-sha2-nistp256",
    "ecdsa-sha2-nistp384",
    "ecdsa-sha2-nistp521",
    "rsa-sha2-512", // RFC 8332
    "rsa-sha2-256", // RFC 8332
    "ssh-rsa",
];

const LEGACY_SERVER_HOST_KEY: &[&str] = &["ssh-dss"];

pub const DEFAULT_CIPHER: &[&str] = &[
    "aes128-gcm@openssh.com",
    "aes256-gcm@openssh.com",
    "aes128-ctr",
    "aes192-ctr",
    "aes256-ctr",
    "chacha20-poly1305@openssh.com",
];

const LEGACY_CIPHER: &[&str] = &["aes256-cbc", "aes192-cbc", "aes128-cbc", "3des-cbc"];

pub const DEFAULT_MAC: &[&str] = &[
    "hmac-sha2-256-etm@openssh.com",
    "hmac-sha2-512-etm@openssh.com",
    "hmac-sha1-etm@openssh.com",
    "hmac-sha2-256",
    "hmac-sha2-512",
    "hmac-sha1",
];

/// Compression when the config sets nothing.
const DEFAULT_COMPRESSION: &[&str] = &["none", "zlib@openssh.com", "zlib"];

/// One algorithm category of ssh_config.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Category {
    Ciphers,
    KexAlgorithms,
    HostKeyAlgorithms,
    Macs,
}

impl Category {
    pub const ALL: [Category; 4] = [
        Category::Ciphers,
        Category::KexAlgorithms,
        Category::HostKeyAlgorithms,
        Category::Macs,
    ];

    /// The ssh_config keyword.
    pub fn keyword(self) -> &'static str {
        match self {
            Category::Ciphers => "Ciphers",
            Category::KexAlgorithms => "KexAlgorithms",
            Category::HostKeyAlgorithms => "HostKeyAlgorithms",
            Category::Macs => "MACs",
        }
    }

    pub fn defaults(self) -> &'static [&'static str] {
        match self {
            Category::Ciphers => DEFAULT_CIPHER,
            Category::KexAlgorithms => DEFAULT_KEX,
            Category::HostKeyAlgorithms => DEFAULT_SERVER_HOST_KEY,
            Category::Macs => DEFAULT_MAC,
        }
    }

    /// Every name the engine can negotiate, defaults first.
    pub fn supported(self) -> Vec<&'static str> {
        let legacy: &[&str] = match self {
            Category::Ciphers => LEGACY_CIPHER,
            Category::KexAlgorithms => LEGACY_KEX,
            Category::HostKeyAlgorithms => LEGACY_SERVER_HOST_KEY,
            Category::Macs => &[],
        };
        self.defaults()
            .iter()
            .chain(legacy.iter())
            .copied()
            .collect()
    }
}

/// Per-keyword algorithm names the engine can negotiate (edit-time validation).
pub fn supported_algorithms() -> Vec<(&'static str, Vec<&'static str>)> {
    Category::ALL
        .iter()
        .map(|c| (c.keyword(), c.supported()))
        .collect()
}

/// How a config list changes the defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ListChange {
    /// A bare list: exactly these, in order.
    Exact(Vec<String>),
    /// `+list`: the defaults, then these.
    Append(Vec<String>),
    /// `^list`: these first, then the rest of the defaults.
    Prepend(Vec<String>),
    /// `-list`: the defaults without these.
    Remove(Vec<String>),
}

impl ListChange {
    /// The final list applied to `defaults`.
    pub fn apply(&self, defaults: &[&str]) -> Vec<String> {
        let defaults: Vec<String> = defaults.iter().map(|s| s.to_string()).collect();
        match self {
            ListChange::Exact(names) => names.clone(),
            ListChange::Append(names) => {
                let mut out = defaults;
                for name in names {
                    if !out.contains(name) {
                        out.push(name.clone());
                    }
                }
                out
            }
            ListChange::Prepend(names) => {
                let mut out = names.clone();
                out.extend(defaults.into_iter().filter(|d| !names.contains(d)));
                out
            }
            ListChange::Remove(names) => defaults
                .into_iter()
                .filter(|d| !names.contains(d))
                .collect(),
        }
    }
}

/// The algorithm settings for one dial.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectionAlgorithms {
    pub cipher: Option<ListChange>,
    pub kex: Option<ListChange>,
    pub server_host_key: Option<ListChange>,
    pub hmac: Option<ListChange>,
    pub compress: Option<Vec<String>>,
    /// User-facing notices about config entries that had to be skipped.
    pub notes: Vec<String>,
}

impl ConnectionAlgorithms {
    /// Whether the config asked for anything at all.
    pub fn is_default(&self) -> bool {
        self.cipher.is_none()
            && self.kex.is_none()
            && self.server_host_key.is_none()
            && self.hmac.is_none()
            && self.compress.is_none()
    }

    fn final_list(&self, category: Category) -> Vec<String> {
        let change = match category {
            Category::Ciphers => &self.cipher,
            Category::KexAlgorithms => &self.kex,
            Category::HostKeyAlgorithms => &self.server_host_key,
            Category::Macs => &self.hmac,
        };
        match change {
            Some(change) => change.apply(category.defaults()),
            None => category.defaults().iter().map(|s| s.to_string()).collect(),
        }
    }

    /// The negotiation lists for the client.
    pub fn preferred(&self) -> Preferred {
        let mut kex: Vec<kex::Name> = self
            .final_list(Category::KexAlgorithms)
            .iter()
            .filter_map(|n| kex::Name::try_from(n.as_str()).ok())
            .collect();
        // Extension markers every client kex list carries (RFC 8308 ext-info, OpenSSH strict
        // kex).
        kex.push(kex::EXTENSION_SUPPORT_AS_CLIENT);
        kex.push(kex::EXTENSION_OPENSSH_STRICT_KEX_AS_CLIENT);
        let key: Vec<Algorithm> = self
            .final_list(Category::HostKeyAlgorithms)
            .iter()
            .filter_map(|n| host_key_algorithm(n))
            .collect();
        let cipher: Vec<cipher::Name> = self
            .final_list(Category::Ciphers)
            .iter()
            .filter_map(|n| cipher::Name::try_from(n.as_str()).ok())
            .collect();
        let mac: Vec<mac::Name> = self
            .final_list(Category::Macs)
            .iter()
            .filter_map(|n| mac::Name::try_from(n.as_str()).ok())
            .collect();
        let compress: Vec<compression::Name> = match &self.compress {
            Some(list) => list
                .iter()
                .filter_map(|n| compression::Name::try_from(n.as_str()).ok())
                .collect(),
            None => DEFAULT_COMPRESSION
                .iter()
                .filter_map(|n| compression::Name::try_from(*n).ok())
                .collect(),
        };
        Preferred {
            kex: Cow::Owned(kex),
            key: Cow::Owned(key),
            host_key_certificates: Cow::Borrowed(&[]),
            cipher: Cow::Owned(cipher),
            mac: Cow::Owned(mac),
            compression: Cow::Owned(compress),
        }
    }
}

/// The host key algorithm for one wire name.
pub fn host_key_algorithm(name: &str) -> Option<Algorithm> {
    Some(match name {
        "ssh-ed25519" => Algorithm::Ed25519,
        "ecdsa-sha2-nistp256" => Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP256,
        },
        "ecdsa-sha2-nistp384" => Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP384,
        },
        "ecdsa-sha2-nistp521" => Algorithm::Ecdsa {
            curve: EcdsaCurve::NistP521,
        },
        "rsa-sha2-512" => Algorithm::Rsa {
            hash: Some(HashAlg::Sha512),
        },
        "rsa-sha2-256" => Algorithm::Rsa {
            hash: Some(HashAlg::Sha256),
        },
        "ssh-rsa" => Algorithm::Rsa { hash: None },
        "ssh-dss" => Algorithm::Dsa,
        _ => return None,
    })
}

/// A HostKeyAlgorithms value that asks the server for one key type first and otherwise keeps the
/// usual order. It is spelled out in full so it means the same to every engine.
pub fn host_key_algorithms_preferring(key_type: &str) -> String {
    let preferred: Vec<&str> = if key_type == "ssh-rsa" {
        vec!["rsa-sha2-512", "rsa-sha2-256", "ssh-rsa"]
    } else {
        vec![key_type]
    };
    preferred
        .iter()
        .copied()
        .chain(
            DEFAULT_SERVER_HOST_KEY
                .iter()
                .copied()
                .filter(|n| !preferred.contains(n)),
        )
        .collect::<Vec<_>>()
        .join(",")
}

/// The raw ssh_config algorithm settings of one host.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AlgorithmSettings {
    pub ciphers: Option<String>,
    pub kex_algorithms: Option<String>,
    pub host_key_algorithms: Option<String>,
    pub macs: Option<String>,
    pub compression: Option<bool>,
}

impl From<&crate::config::ResolvedTarget> for AlgorithmSettings {
    fn from(r: &crate::config::ResolvedTarget) -> Self {
        Self {
            ciphers: r.ciphers.clone(),
            kex_algorithms: r.kex_algorithms.clone(),
            host_key_algorithms: r.host_key_algorithms.clone(),
            macs: r.macs.clone(),
            compression: r.compression,
        }
    }
}

/// An exact list in the config that leaves nothing the engine can offer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("none of the {keyword} in the ssh config (\"{value}\") are supported by the SSH engine")]
pub struct UnsupportedAlgorithms {
    pub keyword: &'static str,
    pub value: String,
}

/// Translates the config lists; fails when an exact list leaves no algorithm to offer at all.
pub fn connection_algorithms(
    settings: &AlgorithmSettings,
) -> Result<ConnectionAlgorithms, UnsupportedAlgorithms> {
    let mut out = ConnectionAlgorithms::default();
    for category in Category::ALL {
        let value = match category {
            Category::Ciphers => &settings.ciphers,
            Category::KexAlgorithms => &settings.kex_algorithms,
            Category::HostKeyAlgorithms => &settings.host_key_algorithms,
            Category::Macs => &settings.macs,
        };
        let Some(value) = value.as_deref().filter(|v| !v.is_empty()) else {
            continue;
        };
        let translated = translate_list(value, category, &mut out.notes)?;
        match category {
            Category::Ciphers => out.cipher = translated,
            Category::KexAlgorithms => out.kex = translated,
            Category::HostKeyAlgorithms => out.server_host_key = translated,
            Category::Macs => out.hmac = translated,
        }
    }
    if let Some(compression) = settings.compression {
        // `Compression yes` prefers zlib the way `ssh -C` does; `no` pins none but stops a
        // zlib-preferring server from winning.
        out.compress = Some(if compression {
            vec!["zlib@openssh.com".into(), "zlib".into(), "none".into()]
        } else {
            vec!["none".into()]
        });
    }
    Ok(out)
}

fn translate_list(
    value: &str,
    category: Category,
    notes: &mut Vec<String>,
) -> Result<Option<ListChange>, UnsupportedAlgorithms> {
    #[derive(PartialEq)]
    enum Mode {
        Exact,
        Append,
        Prepend,
        Remove,
    }
    let (mode, list) = match value.chars().next() {
        Some('+') => (Mode::Append, &value[1..]),
        Some('^') => (Mode::Prepend, &value[1..]),
        Some('-') => (Mode::Remove, &value[1..]),
        _ => (Mode::Exact, value),
    };
    let supported = category.supported();
    let mut names: Vec<String> = Vec::new();
    let mut dropped: Vec<String> = Vec::new();
    for token in list
        .split(',')
        .map(|t| t.trim().to_lowercase())
        .filter(|t| !t.is_empty())
    {
        let matches = expand_entry(&token, &supported);
        // Removing an algorithm the engine never offers is a no-op, not a surprise.
        if matches.is_empty() && mode != Mode::Remove {
            dropped.push(token.clone());
        }
        for name in matches {
            if !names.iter().any(|n| n == name) {
                names.push(name.to_string());
            }
        }
    }
    if !dropped.is_empty() {
        notes.push(format!(
            "{}: skipping {} — not supported by the SSH engine.",
            category.keyword(),
            dropped.join(", ")
        ));
    }
    if mode == Mode::Exact {
        if names.is_empty() {
            return Err(UnsupportedAlgorithms {
                keyword: category.keyword(),
                value: value.to_string(),
            });
        }
        return Ok(Some(ListChange::Exact(names)));
    }
    if names.is_empty() {
        return Ok(None);
    }
    Ok(Some(match mode {
        Mode::Append => ListChange::Append(names),
        Mode::Prepend => ListChange::Prepend(names),
        _ => ListChange::Remove(names),
    }))
}

/// Expand one list entry against the supported table: exact name or `*`/`?` pattern.
fn expand_entry<'a>(token: &str, supported: &[&'a str]) -> Vec<&'a str> {
    if !token.contains(['*', '?']) {
        return supported.iter().copied().filter(|s| *s == token).collect();
    }
    supported
        .iter()
        .copied()
        .filter(|name| glob_match(token, name))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn settings(f: impl FnOnce(&mut AlgorithmSettings)) -> AlgorithmSettings {
        let mut s = AlgorithmSettings::default();
        f(&mut s);
        s
    }

    fn v(names: &[&str]) -> Vec<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn returns_no_override_when_the_config_sets_nothing() {
        let out = connection_algorithms(&AlgorithmSettings::default()).unwrap();
        assert!(out.is_default());
        assert!(out.notes.is_empty());
    }

    #[test]
    fn maps_a_bare_list_to_an_exact_list_order_preserved() {
        let out = connection_algorithms(&settings(|s| {
            s.ciphers = Some("aes128-cbc,aes256-ctr".into())
        }))
        .unwrap();
        assert_eq!(
            out.cipher,
            Some(ListChange::Exact(v(&["aes128-cbc", "aes256-ctr"])))
        );
        assert!(out.notes.is_empty());
    }

    #[test]
    fn maps_prefixes_to_append_prepend_remove_on_the_defaults() {
        let out = connection_algorithms(&settings(|s| {
            s.kex_algorithms =
                Some("+diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1".into());
            s.host_key_algorithms = Some("^ssh-ed25519".into());
            s.macs = Some("-hmac-sha1".into());
        }))
        .unwrap();
        assert_eq!(
            out.kex,
            Some(ListChange::Append(v(&[
                "diffie-hellman-group14-sha1",
                "diffie-hellman-group-exchange-sha1"
            ])))
        );
        assert_eq!(
            out.server_host_key,
            Some(ListChange::Prepend(v(&["ssh-ed25519"])))
        );
        assert_eq!(out.hmac, Some(ListChange::Remove(v(&["hmac-sha1"]))));
    }

    #[test]
    fn expands_wildcard_patterns_against_the_supported_table() {
        let out = connection_algorithms(&settings(|s| s.ciphers = Some("-*cbc".into()))).unwrap();
        assert_eq!(
            out.cipher,
            Some(ListChange::Remove(v(&[
                "aes256-cbc",
                "aes192-cbc",
                "aes128-cbc",
                "3des-cbc"
            ])))
        );
    }

    #[test]
    fn drops_entries_the_engine_does_not_implement_and_reports_them() {
        let out = connection_algorithms(&settings(|s| {
            s.kex_algorithms =
                Some("+sntrup761x25519-sha512@openssh.com,diffie-hellman-group14-sha1".into())
        }))
        .unwrap();
        assert_eq!(
            out.kex,
            Some(ListChange::Append(v(&["diffie-hellman-group14-sha1"])))
        );
        assert_eq!(out.notes.len(), 1);
        assert!(out.notes[0].contains("KexAlgorithms"));
        assert!(out.notes[0].contains("sntrup761x25519-sha512@openssh.com"));
    }

    #[test]
    fn omits_the_category_when_nothing_supported_remains_to_append() {
        let out = connection_algorithms(&settings(|s| {
            s.kex_algorithms = Some("+sntrup761x25519-sha512".into())
        }))
        .unwrap();
        assert!(out.is_default());
        assert_eq!(out.notes.len(), 1);
    }

    #[test]
    fn silently_ignores_removals_of_algorithms_never_offered() {
        let out =
            connection_algorithms(&settings(|s| s.macs = Some("-umac-64@openssh.com".into())))
                .unwrap();
        assert!(out.is_default());
        assert!(out.notes.is_empty());
    }

    #[test]
    fn rejects_an_exact_list_with_no_supported_entries() {
        let err = connection_algorithms(&settings(|s| s.ciphers = Some("chacha8-poly1305".into())))
            .unwrap_err();
        assert!(err.to_string().contains("Ciphers"));
    }

    #[test]
    fn maps_compression_yes_no_and_leaves_unset_alone() {
        let yes = connection_algorithms(&settings(|s| s.compression = Some(true))).unwrap();
        assert_eq!(yes.compress, Some(v(&["zlib@openssh.com", "zlib", "none"])));
        let no = connection_algorithms(&settings(|s| s.compression = Some(false))).unwrap();
        assert_eq!(no.compress, Some(v(&["none"])));
        assert!(
            connection_algorithms(&AlgorithmSettings::default())
                .unwrap()
                .compress
                .is_none()
        );
    }

    #[test]
    fn handles_the_legacy_console_server_recipe_end_to_end() {
        let out = connection_algorithms(&settings(|s| {
            s.ciphers = Some("aes128-cbc".into());
            s.kex_algorithms =
                Some("+diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1".into());
            s.host_key_algorithms = Some("+ssh-rsa".into());
        }))
        .unwrap();
        assert_eq!(out.cipher, Some(ListChange::Exact(v(&["aes128-cbc"]))));
        assert_eq!(
            out.kex,
            Some(ListChange::Append(v(&[
                "diffie-hellman-group14-sha1",
                "diffie-hellman-group-exchange-sha1"
            ])))
        );
        assert_eq!(
            out.server_host_key,
            Some(ListChange::Append(v(&["ssh-rsa"])))
        );
        assert!(out.notes.is_empty());
        // The final lists are what the client offers.
        let preferred = out.preferred();
        assert_eq!(
            preferred
                .cipher
                .iter()
                .map(|c| c.as_ref())
                .collect::<Vec<_>>(),
            vec!["aes128-cbc"]
        );
        assert!(
            preferred
                .kex
                .iter()
                .any(|k| k.as_ref() == "diffie-hellman-group14-sha1")
        );
        assert!(preferred.kex.iter().any(|k| k.as_ref() == "ext-info-c"));
    }

    #[test]
    fn keeps_ssh_dss_and_every_legacy_algorithm_available() {
        let supported = supported_algorithms();
        let get = |k: &str| supported.iter().find(|(kw, _)| *kw == k).unwrap().1.clone();
        assert!(get("HostKeyAlgorithms").contains(&"ssh-dss"));
        for kex in [
            "diffie-hellman-group1-sha1",
            "diffie-hellman-group14-sha1",
            "diffie-hellman-group-exchange-sha1",
        ] {
            assert!(get("KexAlgorithms").contains(&kex));
        }
        assert!(get("Ciphers").contains(&"3des-cbc"));
        assert!(get("MACs").contains(&"hmac-sha1"));
        // Every supported name is one the engine really implements.
        for name in Category::Ciphers.supported() {
            assert!(cipher::Name::try_from(name).is_ok(), "{name}");
        }
        for name in Category::KexAlgorithms.supported() {
            assert!(kex::Name::try_from(name).is_ok(), "{name}");
        }
        for name in Category::Macs.supported() {
            assert!(mac::Name::try_from(name).is_ok(), "{name}");
        }
        for name in Category::HostKeyAlgorithms.supported() {
            assert!(host_key_algorithm(name).is_some(), "{name}");
        }
    }

    #[test]
    fn prepend_moves_an_offered_algorithm_to_the_head() {
        let change = ListChange::Prepend(v(&["ssh-rsa"]));
        let list = change.apply(DEFAULT_SERVER_HOST_KEY);
        assert_eq!(list[0], "ssh-rsa");
        assert_eq!(list.iter().filter(|n| *n == "ssh-rsa").count(), 1);
    }

    #[test]
    fn spells_out_a_host_key_preference() {
        assert_eq!(
            host_key_algorithms_preferring("ssh-rsa"),
            "rsa-sha2-512,rsa-sha2-256,ssh-rsa,ssh-ed25519,ecdsa-sha2-nistp256,ecdsa-sha2-nistp384,ecdsa-sha2-nistp521"
        );
        assert!(
            host_key_algorithms_preferring("ecdsa-sha2-nistp384")
                .starts_with("ecdsa-sha2-nistp384,ssh-ed25519,")
        );
    }
}
