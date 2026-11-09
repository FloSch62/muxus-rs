//! Which host key types the known_hosts files already hold for a host, read before the dial so
//! the server is asked for a key that can be verified.

use super::{KnownHostsStore, entry_matches, host_names, parse_line};

impl KnownHostsStore {
    /// The key types of the plain entries for `host:port`, in file order without repeats. Like
    /// ssh(1), `@revoked` and `@cert-authority` lines do not count.
    pub fn recorded_key_types(&self, host: &str, port: u16) -> Vec<String> {
        let names = host_names(host, port);
        let mut types: Vec<String> = Vec::new();
        for file in self.user_files.iter().chain(self.global_files.iter()) {
            let Ok(text) = std::fs::read(file) else {
                continue;
            };
            let text = String::from_utf8_lossy(&text);
            for raw in text.split('\n') {
                let Some(entry) = parse_line(raw.strip_suffix('\r').unwrap_or(raw)) else {
                    continue;
                };
                if entry.marker.is_none()
                    && !types.iter().any(|t| t == entry.key_type)
                    && entry_matches(entry.hosts, &names)
                {
                    types.push(entry.key_type.to_string());
                }
            }
        }
        types
    }
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn store(dir: &tempfile::TempDir, user: &str, global: &str) -> KnownHostsStore {
        let (user_file, global_file) = (dir.path().join("user"), dir.path().join("global"));
        std::fs::write(&user_file, user).unwrap();
        std::fs::write(&global_file, global).unwrap();
        KnownHostsStore::new(vec![user_file], vec![global_file])
    }

    #[test]
    fn lists_the_key_types_recorded_for_the_host_in_every_file() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(
            &dir,
            "other.example ssh-ed25519 AAAA\n\
             example.com,10.0.0.1 ecdsa-sha2-nistp256 AAAA\n\
             @revoked example.com ssh-ed25519 AAAA\n\
             @cert-authority *.com ssh-ed25519 AAAA\n\
             example.com ecdsa-sha2-nistp256 BBBB\n",
            "*.example.com,example.com ssh-rsa AAAA\n",
        );
        assert_eq!(
            store.recorded_key_types("example.com", 22),
            vec!["ecdsa-sha2-nistp256".to_string(), "ssh-rsa".to_string()]
        );
        assert!(store.recorded_key_types("unknown.example", 22).is_empty());
    }

    #[test]
    fn keeps_ports_apart() {
        let dir = tempfile::tempdir().unwrap();
        let store = store(&dir, "[example.com]:2222 ssh-rsa AAAA\n", "");
        assert_eq!(
            store.recorded_key_types("example.com", 2222),
            vec!["ssh-rsa".to_string()]
        );
        assert!(store.recorded_key_types("example.com", 22).is_empty());
    }

    #[test]
    fn reads_nothing_from_missing_files() {
        let store =
            KnownHostsStore::new(vec![PathBuf::from("/nonexistent/known_hosts")], Vec::new());
        assert!(store.recorded_key_types("example.com", 22).is_empty());
    }
}
