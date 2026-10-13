//! Host key verification against the real OpenSSH files: ~/.ssh/known_hosts (read + write) and
//! /etc/ssh/ssh_known_hosts (read only). Supports plain, wildcard and hashed (|1|salt|hmac) host
//! entries, `[host]:port` notation and the @revoked marker. First contact asks the user (TOFU) and
//! appends a plain entry, like `ssh` with HashKnownHosts=no; accepting a changed key replaces the
//! stale same-type entries the way `ssh-keygen -R` would, keeping a known_hosts.old backup.

use std::path::{Path, PathBuf};

use base64::Engine;
use base64::engine::general_purpose::STANDARD as B64;
use hmac::{Hmac, Mac};
use md5::Md5;
use sha1::Sha1;
use sha2::{Digest, Sha256};

use crate::config::glob_match;
use crate::local_env::LocalEnv;
use crate::paths::path_string;

/// What known_hosts says about a presented key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnownHostVerdict {
    Ok,
    Unknown,
    /// A same-type key is on file for this host; `previous` is its SHA256 fingerprint.
    Changed {
        previous: String,
    },
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Marker {
    Revoked,
    CertAuthority,
}

struct KnownHostLine<'a> {
    marker: Option<Marker>,
    hosts: &'a str,
    key_type: &'a str,
    key_b64: &'a str,
}

/// `~/.ssh/known_hosts`.
pub fn default_known_hosts_path(env: &LocalEnv) -> PathBuf {
    env.ssh_dir().join("known_hosts")
}

pub const GLOBAL_KNOWN_HOSTS: &str = "/etc/ssh/ssh_known_hosts";

/// `SHA256:...` fingerprint, OpenSSH presentation.
pub fn fingerprint_sha256(key: &[u8]) -> String {
    let digest = Sha256::digest(key);
    format!(
        "SHA256:{}",
        base64::engine::general_purpose::STANDARD_NO_PAD.encode(digest)
    )
}

/// `MD5:...` fingerprint, as `ssh-keygen -E md5 -l` prints it.
pub fn fingerprint_md5(key: &[u8]) -> String {
    let digest = Md5::digest(key);
    let hex: Vec<String> = digest.iter().map(|b| format!("{b:02x}")).collect();
    format!("MD5:{}", hex.join(":"))
}

/// The key's fingerprint in the same hash as a canonical link fingerprint (see
/// `normalize_host_key_fingerprint`), for comparing and for messages.
pub fn fingerprint_like(key: &[u8], expected: &str) -> String {
    if expected.contains("MD5:") {
        fingerprint_md5(key)
    } else {
        fingerprint_sha256(key)
    }
}

/// Whether a host key is the one a canonical link fingerprint names.
pub fn host_key_matches_fingerprint(key: &[u8], expected: &str) -> bool {
    let (key_type, print) = match expected.find(' ') {
        Some(space) => (Some(&expected[..space]), &expected[space + 1..]),
        None => (None, expected),
    };
    if let Some(key_type) = key_type
        && host_key_type(key) != key_type
    {
        return false;
    }
    fingerprint_like(key, expected) == print
}

/// "ssh-ed25519", "ecdsa-sha2-nistp256", ...: the leading string of the key blob.
pub fn host_key_type(key: &[u8]) -> String {
    let Some(len) = key
        .get(..4)
        .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]) as usize)
    else {
        return "unknown".to_string();
    };
    // Node's subarray clamps an overlong length to the buffer.
    let end = (4 + len).min(key.len());
    key[4..end].iter().map(|&b| b as char).collect()
}

/// The user's and the system's known_hosts files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownHostsStore {
    /// New entries land in the first user file; the rest are read-only.
    user_files: Vec<PathBuf>,
    global_files: Vec<PathBuf>,
}

impl KnownHostsStore {
    /// Lists follow UserKnownHostsFile/GlobalKnownHostsFile: an empty list means `none`.
    pub fn new(user_files: Vec<PathBuf>, global_files: Vec<PathBuf>) -> Self {
        Self {
            user_files,
            global_files,
        }
    }

    /// `~/.ssh/known_hosts` plus `/etc/ssh/ssh_known_hosts`.
    pub fn default_for(env: &LocalEnv) -> Self {
        Self::new(
            vec![default_known_hosts_path(env)],
            vec![PathBuf::from(GLOBAL_KNOWN_HOSTS)],
        )
    }

    pub fn user_files(&self) -> &[PathBuf] {
        &self.user_files
    }

    pub fn global_files(&self) -> &[PathBuf] {
        &self.global_files
    }

    /// Checks a presented key blob for `host:port`.
    pub fn verify(&self, host: &str, port: u16, key: &[u8]) -> KnownHostVerdict {
        let names = host_names(host, port);
        let key_type = host_key_type(key);
        let key_b64 = B64.encode(key);
        let mut previous: Option<String> = None;

        for file in self.user_files.iter().chain(self.global_files.iter()) {
            let Ok(text) = std::fs::read(file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&text);
            for raw in text.split('\n') {
                let Some(entry) = parse_line(raw.strip_suffix('\r').unwrap_or(raw)) else {
                    continue;
                };
                if entry.marker == Some(Marker::CertAuthority) {
                    continue; // CA validation is out of scope
                }
                if entry.key_type != key_type || !entry_matches(entry.hosts, &names) {
                    continue;
                }
                if entry.key_b64 == key_b64 {
                    if entry.marker == Some(Marker::Revoked) {
                        return KnownHostVerdict::Revoked;
                    }
                    return KnownHostVerdict::Ok;
                }
                if previous.is_none() {
                    let old = B64.decode(entry.key_b64).unwrap_or_default();
                    previous = Some(fingerprint_sha256(&old));
                }
            }
        }
        match previous {
            Some(previous) => KnownHostVerdict::Changed { previous },
            None => KnownHostVerdict::Unknown,
        }
    }

    /// Record an accepted key: drop stale same-type entries for the host (the `ssh-keygen -R`
    /// part, backing up to known_hosts.old), then append.
    pub fn record(&self, host: &str, port: u16, key: &[u8]) -> std::io::Result<()> {
        let Some(target) = self.user_files.first() else {
            return Ok(()); // UserKnownHostsFile none: nowhere to remember it
        };
        // OpenSSH users commonly select the null device to accept host keys without persisting
        // them. Our atomic sibling-file rewrite cannot apply there, and the device already
        // provides the intended discard semantics.
        if is_null_device(target) {
            return Ok(());
        }
        let names = host_names(host, port);
        let key_type = host_key_type(key);
        if let Some(dir) = target.parent() {
            create_private_dir(dir)?;
        }

        let mut lines: Vec<String> = Vec::new();
        if let Ok(bytes) = std::fs::read(target) {
            let text = String::from_utf8_lossy(&bytes).into_owned();
            write_private(
                &PathBuf::from(format!("{}.old", path_string(target))),
                text.as_bytes(),
            )?;
            lines = crate::config::split_lines(&text);
            while lines.last().is_some_and(String::is_empty) {
                lines.pop();
            }
        }

        let mut kept: Vec<String> = lines
            .into_iter()
            .filter(|line| match parse_line(line) {
                None => true,
                Some(entry) => {
                    entry.marker.is_some()
                        || entry.key_type != key_type
                        || !entry_matches(entry.hosts, &names)
                }
            })
            .collect();
        kept.push(format!("{} {key_type} {}", names[0], B64.encode(key)));

        let tmp = PathBuf::from(format!("{}.muxus.tmp", path_string(target)));
        write_private(&tmp, format!("{}\n", kept.join("\n")).as_bytes())?;
        std::fs::rename(&tmp, target)
    }
}

fn is_null_device(path: &Path) -> bool {
    #[cfg(windows)]
    {
        path.as_os_str().eq_ignore_ascii_case(r"\\.\nul")
    }
    #[cfg(not(windows))]
    {
        path == Path::new("/dev/null")
    }
}

fn create_private_dir(dir: &Path) -> std::io::Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(dir)
}

/// Writes `data` to a file created with mode 0600 (existing files keep their mode, as Node's
/// `writeFileSync` does).
fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(data)
}

/// The names OpenSSH would store/check for this host+port.
fn host_names(host: &str, port: u16) -> Vec<String> {
    let h = host.to_lowercase();
    if port == 22 {
        vec![h.clone(), format!("[{h}]:22")]
    } else {
        vec![format!("[{h}]:{port}")]
    }
}

fn parse_line(raw: &str) -> Option<KnownHostLine<'_>> {
    let line = raw.trim();
    if line.is_empty() || line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace().peekable();
    let marker = match fields.peek() {
        Some(&"@revoked") => Some(Marker::Revoked),
        Some(&"@cert-authority") => Some(Marker::CertAuthority),
        _ => None,
    };
    if marker.is_some() {
        fields.next();
    }
    let hosts = fields.next()?;
    let key_type = fields.next()?;
    let key_b64 = fields.next()?;
    Some(KnownHostLine {
        marker,
        hosts,
        key_type,
        key_b64,
    })
}

/// Match one entry's host field (comma-separated patterns or a |1| hash).
fn entry_matches(hosts_field: &str, names: &[String]) -> bool {
    if hosts_field.starts_with('|') {
        return hashed_matches(hosts_field, names);
    }
    let mut matched = false;
    for pattern in hosts_field.split(',') {
        let p = pattern.to_lowercase();
        if p.is_empty() {
            continue;
        }
        if let Some(negated) = p.strip_prefix('!') {
            if names.iter().any(|n| glob_match(negated, n)) {
                return false;
            }
        } else if names.iter().any(|n| glob_match(&p, n)) {
            matched = true;
        }
    }
    matched
}

/// `|1|base64(salt)|base64(hmac-sha1(salt, host))`
fn hashed_matches(field: &str, names: &[String]) -> bool {
    let parts: Vec<&str> = field.split('|').collect();
    if parts.len() != 4 || parts[1] != "1" {
        return false;
    }
    let (Ok(salt), Ok(hash)) = (B64.decode(parts[2]), B64.decode(parts[3])) else {
        return false;
    };
    names.iter().any(|name| {
        let Ok(mut mac) = Hmac::<Sha1>::new_from_slice(&salt) else {
            return false;
        };
        mac.update(name.as_bytes());
        mac.verify_slice(&hash).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::RngCore;

    struct Files {
        dir: tempfile::TempDir,
        counter: std::cell::Cell<usize>,
    }

    impl Files {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().unwrap(),
                counter: std::cell::Cell::new(0),
            }
        }

        fn fresh(&self) -> PathBuf {
            let n = self.counter.get();
            self.counter.set(n + 1);
            self.dir.path().join(format!("known_hosts-{n}"))
        }

        fn missing(&self) -> PathBuf {
            self.dir.path().join("no-global-file")
        }

        fn store(&self, user: Vec<PathBuf>) -> KnownHostsStore {
            KnownHostsStore::new(user, vec![self.missing()])
        }
    }

    /// A syntactically valid SSH key blob: uint32 length + type + random payload.
    fn make_key(key_type: &str) -> Vec<u8> {
        let mut out = (key_type.len() as u32).to_be_bytes().to_vec();
        out.extend_from_slice(key_type.as_bytes());
        let mut payload = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut payload);
        out.extend_from_slice(&payload);
        out
    }

    fn hashed_entry(host: &str, key: &[u8]) -> String {
        let mut salt = [0u8; 20];
        rand::thread_rng().fill_bytes(&mut salt);
        let mut mac = Hmac::<Sha1>::new_from_slice(&salt).unwrap();
        mac.update(host.as_bytes());
        let digest = mac.finalize().into_bytes();
        format!(
            "|1|{}|{} {} {}",
            B64.encode(salt),
            B64.encode(digest),
            host_key_type(key),
            B64.encode(key)
        )
    }

    fn read(p: &Path) -> String {
        std::fs::read_to_string(p).unwrap()
    }

    #[test]
    fn reports_unknown_hosts_then_ok_after_record() {
        let f = Files::new();
        let store = f.store(vec![f.fresh()]);
        let key = make_key("ssh-ed25519");
        assert_eq!(
            store.verify("example.com", 22, &key),
            KnownHostVerdict::Unknown
        );
        store.record("example.com", 22, &key).unwrap();
        assert_eq!(store.verify("example.com", 22, &key), KnownHostVerdict::Ok);
    }

    #[test]
    fn reads_every_configured_user_file_but_records_into_the_first() {
        let f = Files::new();
        let (primary, extra) = (f.fresh(), f.fresh());
        let key = make_key("ssh-ed25519");
        std::fs::write(
            &extra,
            format!(
                "readonly.example {} {}\n",
                host_key_type(&key),
                B64.encode(&key)
            ),
        )
        .unwrap();
        let store = f.store(vec![primary.clone(), extra.clone()]);
        assert_eq!(
            store.verify("readonly.example", 22, &key),
            KnownHostVerdict::Ok
        );
        store.record("new.example", 22, &key).unwrap();
        assert!(read(&primary).contains("new.example"));
        assert!(!read(&extra).contains("new.example"));
    }

    #[test]
    fn user_known_hosts_file_none_matches_nothing_and_record_is_a_no_op() {
        let f = Files::new();
        let store = f.store(vec![]);
        let key = make_key("ssh-ed25519");
        assert_eq!(
            store.verify("example.com", 22, &key),
            KnownHostVerdict::Unknown
        );
        store.record("example.com", 22, &key).unwrap();
        assert_eq!(
            store.verify("example.com", 22, &key),
            KnownHostVerdict::Unknown
        );
    }

    #[cfg(unix)]
    #[test]
    fn user_known_hosts_file_set_to_the_null_device_discards_accepted_keys() {
        let f = Files::new();
        let store = f.store(vec![PathBuf::from("/dev/null")]);
        let key = make_key("ssh-ed25519");
        store.record("example.com", 22, &key).unwrap();
        assert_eq!(
            store.verify("example.com", 22, &key),
            KnownHostVerdict::Unknown
        );
    }

    #[test]
    fn stores_non_22_ports_in_bracket_notation() {
        let f = Files::new();
        let file = f.fresh();
        let store = f.store(vec![file.clone()]);
        let key = make_key("ssh-ed25519");
        store.record("example.com", 2222, &key).unwrap();
        assert!(read(&file).contains("[example.com]:2222 ssh-ed25519"));
        assert_eq!(
            store.verify("example.com", 2222, &key),
            KnownHostVerdict::Ok
        );
        assert_eq!(
            store.verify("example.com", 22, &key),
            KnownHostVerdict::Unknown
        );
    }

    #[test]
    fn matches_hashed_and_wildcard_entries() {
        let f = Files::new();
        let file = f.fresh();
        let key = make_key("ssh-ed25519");
        let key2 = make_key("ssh-rsa");
        std::fs::write(
            &file,
            format!(
                "{}\n*.wild.example.com {} {}\n",
                hashed_entry("secret.example.com", &key),
                host_key_type(&key2),
                B64.encode(&key2)
            ),
        )
        .unwrap();
        let store = f.store(vec![file]);
        assert_eq!(
            store.verify("secret.example.com", 22, &key),
            KnownHostVerdict::Ok
        );
        assert_eq!(
            store.verify("a.wild.example.com", 22, &key2),
            KnownHostVerdict::Ok
        );
    }

    #[test]
    fn flags_a_same_type_key_change_with_the_previous_fingerprint() {
        let f = Files::new();
        let store = f.store(vec![f.fresh()]);
        let old_key = make_key("ssh-ed25519");
        store.record("example.com", 22, &old_key).unwrap();
        let new_key = make_key("ssh-ed25519");
        assert_eq!(
            store.verify("example.com", 22, &new_key),
            KnownHostVerdict::Changed {
                previous: fingerprint_sha256(&old_key)
            }
        );
        // A different key *type* is first contact, not a change.
        assert_eq!(
            store.verify("example.com", 22, &make_key("ssh-rsa")),
            KnownHostVerdict::Unknown
        );
    }

    #[test]
    fn replaces_stale_entries_on_record_keeping_a_dot_old_backup() {
        let f = Files::new();
        let file = f.fresh();
        let store = f.store(vec![file.clone()]);
        let (old_key, new_key) = (make_key("ssh-ed25519"), make_key("ssh-ed25519"));
        store.record("example.com", 22, &old_key).unwrap();
        store.record("example.com", 22, &new_key).unwrap();
        let text = read(&file);
        assert!(!text.contains(&B64.encode(&old_key)));
        assert!(text.contains(&B64.encode(&new_key)));
        assert!(
            read(&PathBuf::from(format!("{}.old", path_string(&file))))
                .contains(&B64.encode(&old_key))
        );
        assert_eq!(
            store.verify("example.com", 22, &new_key),
            KnownHostVerdict::Ok
        );
    }

    #[test]
    fn honors_revoked_markers() {
        let f = Files::new();
        let file = f.fresh();
        let key = make_key("ssh-ed25519");
        std::fs::write(
            &file,
            format!(
                "@revoked example.com {} {}\n",
                host_key_type(&key),
                B64.encode(&key)
            ),
        )
        .unwrap();
        assert_eq!(
            f.store(vec![file]).verify("example.com", 22, &key),
            KnownHostVerdict::Revoked
        );
    }

    #[test]
    fn consults_the_global_file_read_only() {
        let f = Files::new();
        let global = f.fresh();
        let key = make_key("ssh-ed25519");
        std::fs::write(
            &global,
            format!(
                "corp.example.com {} {}\n",
                host_key_type(&key),
                B64.encode(&key)
            ),
        )
        .unwrap();
        let store = KnownHostsStore::new(vec![f.fresh()], vec![global]);
        assert_eq!(
            store.verify("corp.example.com", 22, &key),
            KnownHostVerdict::Ok
        );
    }

    /// host-key-pin.test.ts: "compares SHA256, MD5 and key-typed fingerprints".
    #[test]
    fn compares_sha256_md5_and_key_typed_fingerprints() {
        let key = make_key("ssh-ed25519");
        let sha = fingerprint_sha256(&key);
        let md5 = fingerprint_md5(&key);
        assert!(host_key_matches_fingerprint(&key, &sha));
        assert!(host_key_matches_fingerprint(&key, &md5));
        assert!(host_key_matches_fingerprint(
            &key,
            &format!("ssh-ed25519 {sha}")
        ));
        assert!(!host_key_matches_fingerprint(
            &key,
            &format!("ssh-rsa {sha}")
        ));
        assert!(!host_key_matches_fingerprint(
            &make_key("ssh-ed25519"),
            &sha
        ));
        assert_eq!(md5.len(), "MD5:".len() + 16 * 3 - 1);
        assert!(!sha.ends_with('='));
    }
}
