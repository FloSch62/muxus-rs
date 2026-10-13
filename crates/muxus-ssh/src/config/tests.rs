//! Port of tests/unit/server/ssh-config.test.ts.

use crate::types::ConfigForwardExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use sha1::{Digest, Sha1};

use super::*;
use crate::types::{ConfigForward, ForwardType, RequestTty, StrictHostKeyChecking};

pub(crate) struct Fixture {
    pub dir: tempfile::TempDir,
    pub env: LocalEnv,
    counter: std::cell::Cell<usize>,
}

impl Fixture {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let env = LocalEnv {
            home: dir.path().join("home"),
            username: "localuser".to_string(),
            hostname: "laptop.example.lan".to_string(),
            uid: 4242,
        };
        Self {
            dir,
            env,
            counter: std::cell::Cell::new(0),
        }
    }

    pub fn write(&self, content: &str) -> PathBuf {
        let n = self.counter.get();
        self.counter.set(n + 1);
        self.write_named(content, &format!("config-{n}"))
    }

    pub fn write_named(&self, content: &str, name: &str) -> PathBuf {
        let file = self.dir.path().join(name);
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, content).unwrap();
        file
    }

    pub fn doc(&self, content: &str) -> ConfigDocument {
        load_config_document(&self.write(content), &self.env)
    }

    pub fn hosts(&self, content: &str) -> Vec<SshHostEntry> {
        list_hosts(&self.doc(content), None)
    }

    pub fn home(&self, rest: &[&str]) -> String {
        let mut p = self.env.home.clone();
        for part in rest {
            p.push(part);
        }
        path_string(&p)
    }
}

fn lines(parts: &[&str]) -> String {
    parts.join("\n")
}

fn s(v: &[&str]) -> Vec<String> {
    v.iter().map(|x| x.to_string()).collect()
}

// --- splitArgs ---

#[test]
fn split_args_accepts_single_and_double_quotes_while_preserving_windows_backslashes() {
    let windows_key = r"C:\Users\toweber\OneDrive - Nokia\NPI\SSH Key\keypair\toweber";
    assert_eq!(
        split_args(&format!("'{windows_key}' \"double quoted\" plain")).unwrap(),
        s(&[windows_key, "double quoted", "plain"])
    );
}

#[test]
fn split_args_supports_the_basic_escapes_openssh_accepts() {
    assert_eq!(
        split_args(r"escaped\ space escaped\'quote C:\Users\toweber").unwrap(),
        s(&["escaped space", "escaped'quote", r"C:\Users\toweber"])
    );
}

#[test]
fn split_args_rejects_unterminated_quotes() {
    assert_eq!(
        split_args("'unterminated").unwrap_err().to_string(),
        "invalid quotes"
    );
}

// --- listHosts ---

#[test]
fn lists_concrete_hosts_with_block_options_and_resolved_settings() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "# Production web server",
        "# behind the LB",
        "Host web",
        "  HostName web.example.com",
        "  User deploy",
        "  Port 2222",
    ]));
    assert_eq!(hosts.len(), 1);
    let web = &hosts[0];
    assert_eq!(web.alias, "web");
    assert_eq!(
        web.description.as_deref(),
        Some("Production web server\nbehind the LB")
    );
    assert_eq!(web.options.hostname.as_deref(), Some("web.example.com"));
    assert_eq!(web.options.user.as_deref(), Some("deploy"));
    assert_eq!(web.options.port, Some(2222));
    assert_eq!(web.resolved.hostname, "web.example.com");
    assert_eq!(web.resolved.user.as_deref(), Some("deploy"));
    assert_eq!(web.resolved.port, 2222);
    assert!(web.resolved.proxy_jump.is_empty());
    assert!(web.resolved.forwards.is_empty());
}

#[test]
fn keeps_multi_alias_blocks_as_one_entry_and_skips_wildcard_patterns() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host db db.example.com backup-*",
        "  User admin",
        "",
        "Host *",
        "  Port 2200",
    ]));
    assert_eq!(hosts.len(), 1);
    assert_eq!(hosts[0].alias, "db");
    assert_eq!(hosts[0].aliases, s(&["db", "db.example.com"]));
    assert_eq!(hosts[0].resolved.port, 2200); // Host * default applied
    assert_eq!(hosts[0].options.port, None); // ...but not written in the block
}

#[test]
fn collects_unmodeled_options_into_extras_order_preserved() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host a",
        "  Compression yes",
        "  HostName a.example.com",
        "  ServerAliveCountMax 5",
    ]));
    assert_eq!(
        hosts[0].options.extras,
        Some(vec![
            crate::types::extra("Compression", "yes"),
            crate::types::extra("ServerAliveCountMax", "5")
        ])
    );
}

#[test]
fn parses_forwards_proxy_jumps_and_auth_options() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host app",
        "  ProxyJump bastion,ops@edge:2200",
        "  IdentityFile ~/.ssh/app_key",
        "  CertificateFile ~/.ssh/app_key-cert.pub",
        "  IdentitiesOnly yes",
        "  ForwardAgent yes",
        "  LocalForward 8080 localhost:80",
        "  LocalForward 127.0.0.1:8443 [::1]:443",
        "  RemoteForward 9000 127.0.0.1:3000",
        "  DynamicForward 1080",
    ]));
    let app = &hosts[0];
    assert_eq!(
        app.options.proxy_jump,
        Some(s(&["bastion", "ops@edge:2200"]))
    );
    assert_eq!(app.options.identity_files, Some(s(&["~/.ssh/app_key"])));
    assert_eq!(
        app.options.certificate_files,
        Some(s(&["~/.ssh/app_key-cert.pub"]))
    );
    assert_eq!(app.options.identities_only, Some(true));
    assert_eq!(app.options.forward_agent, Some(true));
    assert_eq!(
        app.options.forwards,
        Some(vec![
            ConfigForward::to(ForwardType::Local, 8080, "localhost", 80),
            ConfigForward::to(ForwardType::Local, 8443, "::1", 443),
            ConfigForward::to(ForwardType::Remote, 9000, "127.0.0.1", 3000),
            ConfigForward::dynamic(1080),
        ])
    );
    assert_eq!(
        app.resolved.identity_files,
        vec![f.home(&[".ssh", "app_key"])]
    );
    assert_eq!(
        app.resolved.certificate_files,
        vec![f.home(&[".ssh", "app_key-cert.pub"])]
    );
}

#[test]
fn keeps_forward_x11_unset_unless_a_matching_block_sets_it() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host gui",
        "  ForwardX11 yes",
        "Host plain",
        "  HostName plain.example.test",
        "Host console",
        "  ForwardX11 no",
        "  ForwardX11 yes",
    ]));
    let (gui, plain, console) = (&hosts[0], &hosts[1], &hosts[2]);
    assert_eq!(gui.options.forward_x11, Some(true));
    assert_eq!(gui.resolved.forward_x11, Some(true));
    assert_eq!(plain.options.forward_x11, None);
    assert_eq!(plain.resolved.forward_x11, None);
    // First obtained value wins; the repeat is preserved as an extra.
    assert_eq!(console.options.forward_x11, Some(false));
    assert_eq!(
        console.options.extras,
        Some(vec![crate::types::extra("ForwardX11", "yes")])
    );
    assert_eq!(console.resolved.forward_x11, Some(false));
}

#[test]
fn resolves_a_single_quoted_windows_identity_path_with_spaces() {
    let f = Fixture::new();
    let windows_key =
        r"C:\Users\toweber\OneDrive - Nokia\NPI\SSH Key\keypair\securecrt_created\toweber";
    let hosts = f.hosts(&lines(&[
        "Host windows",
        &format!("  IdentityFile '{windows_key}'"),
    ]));
    assert_eq!(hosts[0].options.identity_files, Some(s(&[windows_key])));
    assert_eq!(hosts[0].resolved.identity_files, s(&[windows_key]));
}

#[test]
fn reports_and_skips_a_config_line_with_unterminated_quotes() {
    let f = Fixture::new();
    let file = f.write(&lines(&["Host broken", "  IdentityFile 'C:\\broken key"]));
    let doc = load_config_document(&file, &f.env);
    assert_eq!(
        doc.error,
        Some(format!("{} line 2: invalid quotes", path_string(&file)))
    );
    assert!(resolve_host(&doc, "broken", None).identity_files.is_empty());
}

#[test]
fn models_proxy_command_as_a_first_class_block_option() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host cloud",
        "  ProxyCommand cloudflared access ssh --hostname %h",
    ]));
    assert_eq!(
        hosts[0].options.proxy_command.as_deref(),
        Some("cloudflared access ssh --hostname %h")
    );
    assert_eq!(hosts[0].options.extras, None);
}

#[test]
fn models_agent_host_verification_and_startup_behavior_as_first_class_options() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host console",
        "  IdentityAgent ${ONEPASSWORD_SSH_AUTH_SOCK}",
        "  StrictHostKeyChecking accept-new",
        "  RemoteCommand tmux new -A -s main",
        "  RequestTTY yes",
    ]));
    let o = &hosts[0].options;
    assert_eq!(
        o.identity_agent.as_deref(),
        Some("${ONEPASSWORD_SSH_AUTH_SOCK}")
    );
    assert_eq!(
        o.strict_host_key_checking,
        Some(StrictHostKeyChecking::AcceptNew)
    );
    assert_eq!(o.remote_command.as_deref(), Some("tmux new -A -s main"));
    assert_eq!(o.request_tty, Some(RequestTty::Yes));
    assert_eq!(
        hosts[0].resolved.identity_agent.as_deref(),
        Some("${ONEPASSWORD_SSH_AUTH_SOCK}")
    );
    assert_eq!(o.extras, None);
}

#[test]
fn maps_pubkey_authentication_no_to_password_only() {
    let f = Fixture::new();
    let hosts = f.hosts(&lines(&[
        "Host legacy",
        "  PubkeyAuthentication no",
        "  PreferredAuthentications keyboard-interactive,password",
    ]));
    assert_eq!(hosts[0].options.password_only, Some(true));
    assert_eq!(hosts[0].options.extras, None);
    assert!(hosts[0].resolved.password_only);
}

// --- resolveHost ---

#[test]
fn applies_first_obtained_wins_across_matching_blocks_and_globals() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "User global-user",
        "",
        "Host web",
        "  Port 2222",
        "",
        "Host web",
        "  Port 9999",
        "  HostName real.example.com",
        "",
        "Host *",
        "  User star-user",
    ]));
    let r = resolve_host(&doc, "web", None);
    assert_eq!(r.user.as_deref(), Some("global-user")); // top-level option read first
    assert_eq!(r.port, 2222); // first block wins
    assert_eq!(r.hostname, "real.example.com"); // first obtained anywhere
}

#[test]
fn accumulates_identity_file_across_blocks_and_dedupes() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host web",
        "  IdentityFile /a",
        "",
        "Host *",
        "  IdentityFile /b",
        "  IdentityFile /a",
    ]));
    assert_eq!(
        resolve_host(&doc, "web", None).identity_files,
        s(&["/a", "/b"])
    );
}

#[test]
fn accumulates_certificate_file_and_resolves_proxy_command() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host web",
        "  CertificateFile ~/.ssh/web-cert.pub",
        "  ProxyCommand connect-proxy %h %p",
        "",
        "Host *",
        "  CertificateFile /shared-cert.pub",
        "  CertificateFile ~/.ssh/web-cert.pub",
        "  ProxyJump ignored-because-proxy-command-won",
    ]));
    let resolved = resolve_host(&doc, "web", None);
    assert_eq!(
        resolved.certificate_files,
        vec![f.home(&[".ssh", "web-cert.pub"]), "/shared-cert.pub".into()]
    );
    assert_eq!(
        resolved.proxy_command.as_deref(),
        Some("connect-proxy %h %p")
    );
    assert!(resolved.proxy_jump.is_empty());
}

#[test]
fn lets_proxy_jump_win_when_it_is_obtained_before_proxy_command() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host web",
        "  ProxyJump bastion",
        "",
        "Host *",
        "  ProxyCommand ignored %h %p",
    ]));
    let resolved = resolve_host(&doc, "web", None);
    assert_eq!(resolved.proxy_jump, s(&["bastion"]));
    assert_eq!(resolved.proxy_command, None);
}

#[test]
fn expands_percent_h_in_hostname_and_defaults_hostname_to_the_alias() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host node-1 node-2",
        "  HostName %h.cluster.internal",
    ]));
    assert_eq!(
        resolve_host(&doc, "node-2", None).hostname,
        "node-2.cluster.internal"
    );
    assert_eq!(resolve_host(&doc, "unknown", None).hostname, "unknown");
}

#[test]
fn honors_negated_patterns() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&["Host prod-* !prod-3", "  User produser"]));
    assert_eq!(
        resolve_host(&doc, "prod-1", None).user.as_deref(),
        Some("produser")
    );
    assert_eq!(resolve_host(&doc, "prod-3", None).user, None);
}

#[test]
fn skips_match_blocks_and_picks_up_options_after_them() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host web",
        "  Port 2222",
        "",
        "Match host web",
        "  User matched",
        "",
        "Host web2",
        "  User plain",
    ]));
    assert_eq!(resolve_host(&doc, "web", None).user, None);
    assert_eq!(
        resolve_host(&doc, "web2", None).user.as_deref(),
        Some("plain")
    );
}

#[test]
fn reads_connect_timeout_and_server_alive_interval() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host slow",
        "  ConnectTimeout 45",
        "  ServerAliveInterval 30",
    ]));
    let r = resolve_host(&doc, "slow", None);
    assert_eq!(r.connect_timeout, Some(45.0));
    assert_eq!(r.server_alive_interval, Some(30.0));
}

#[test]
fn resolves_the_dial_time_tunables_muxus_applies_from_advanced_options() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host lab",
        "  Compression yes",
        "  IdentityAgent ~/.1password/agent.sock",
        "  PasswordAuthentication no",
        "  KbdInteractiveAuthentication no",
        "  RemoteCommand tmux new -A -s main",
        "  RequestTTY yes",
        "  StrictHostKeyChecking accept-new",
        "  UserKnownHostsFile ~/.ssh/lab_hosts ~/.ssh/extra_hosts",
    ]));
    let r = resolve_host(&doc, "lab", None);
    assert_eq!(r.compression, Some(true));
    assert_eq!(
        r.identity_agent,
        Some(f.home(&[".1password", "agent.sock"]))
    );
    assert_eq!(r.password_authentication, Some(false));
    assert_eq!(r.kbd_interactive_authentication, Some(false));
    assert_eq!(r.remote_command.as_deref(), Some("tmux new -A -s main"));
    assert_eq!(r.request_tty, Some(RequestTty::Yes));
    assert_eq!(
        r.strict_host_key_checking,
        Some(StrictHostKeyChecking::AcceptNew)
    );
    assert_eq!(
        r.user_known_hosts_files,
        Some(vec![
            f.home(&[".ssh", "lab_hosts"]),
            f.home(&[".ssh", "extra_hosts"])
        ])
    );
    assert_eq!(r.global_known_hosts_files, None);
}

#[test]
fn keeps_identity_agent_indirections_verbatim_and_maps_none_known_hosts_to_empty() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host a",
        "  IdentityAgent $CUSTOM_SOCK",
        "  UserKnownHostsFile none",
        "",
        "Host b",
        "  IdentityAgent ${CUSTOM_SOCK}",
        "",
        "Host c",
        "  IdentityAgent none",
    ]));
    assert_eq!(
        resolve_host(&doc, "a", None).identity_agent.as_deref(),
        Some("$CUSTOM_SOCK")
    );
    assert_eq!(
        resolve_host(&doc, "a", None).user_known_hosts_files,
        Some(vec![])
    );
    assert_eq!(
        resolve_host(&doc, "b", None).identity_agent.as_deref(),
        Some("${CUSTOM_SOCK}")
    );
    assert_eq!(
        resolve_host(&doc, "c", None).identity_agent.as_deref(),
        Some("none")
    );
}

#[test]
fn reads_security_key_provider_first_obtained_keeping_var_indirections() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host fido",
        "  SecurityKeyProvider $SK_LIB",
        "",
        "Host *",
        "  SecurityKeyProvider internal",
    ]));
    assert_eq!(
        resolve_host(&doc, "fido", None)
            .security_key_provider
            .as_deref(),
        Some("$SK_LIB")
    );
    assert_eq!(
        resolve_host(&doc, "other", None)
            .security_key_provider
            .as_deref(),
        Some("internal")
    );
}

#[test]
fn leaves_unset_flags_undefined_and_rejects_invalid_choice_values() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host plain",
        "  RequestTTY sometimes",
        "  StrictHostKeyChecking maybe",
    ]));
    let r = resolve_host(&doc, "plain", None);
    assert_eq!(r.compression, None);
    assert_eq!(r.password_authentication, None);
    assert_eq!(r.kbd_interactive_authentication, None);
    assert_eq!(r.request_tty, None);
    assert_eq!(r.strict_host_key_checking, None);
}

#[test]
fn honors_the_legacy_challenge_response_authentication_spelling() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host old",
        "  ChallengeResponseAuthentication no",
    ]));
    assert_eq!(
        resolve_host(&doc, "old", None).kbd_interactive_authentication,
        Some(false)
    );
}

#[test]
fn preserves_first_obtained_wins_across_keyboard_interactive_auth_aliases() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host legacy-first",
        "  ChallengeResponseAuthentication no",
        "",
        "Host modern-first",
        "  KbdInteractiveAuthentication no",
        "",
        "Host *",
        "  KbdInteractiveAuthentication yes",
        "  ChallengeResponseAuthentication yes",
    ]));
    assert_eq!(
        resolve_host(&doc, "legacy-first", None).kbd_interactive_authentication,
        Some(false)
    );
    assert_eq!(
        resolve_host(&doc, "modern-first", None).kbd_interactive_authentication,
        Some(false)
    );
}

#[test]
fn accumulates_set_env_first_wins_per_variable_and_send_env_patterns_in_order() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host env",
        "  SetEnv FOO=bar BAZ=qux",
        "  SendEnv LANG LC_*",
        "",
        "Host *",
        "  SetEnv FOO=global EXTRA=yes",
        "  SendEnv -LC_ALL EDITOR",
    ]));
    let r = resolve_host(&doc, "env", None);
    let expected: BTreeMap<String, String> = [("FOO", "bar"), ("BAZ", "qux"), ("EXTRA", "yes")]
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    assert_eq!(r.set_env, expected);
    assert_eq!(r.send_env, s(&["LANG", "LC_*", "-LC_ALL", "EDITOR"]));
}

#[test]
fn preserves_quoted_spaces_in_set_env_assignments() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host env",
        "  SetEnv GREETING=\"hello world\" EMPTY=\"\" PLAIN=value",
    ]));
    let expected: BTreeMap<String, String> = [
        ("GREETING", "hello world"),
        ("EMPTY", ""),
        ("PLAIN", "value"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(resolve_host(&doc, "env", None).set_env, expected);
}

#[test]
fn treats_remote_command_none_as_disabled() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&["Host shell", "  RemoteCommand NoNe"]));
    assert_eq!(resolve_host(&doc, "shell", None).remote_command, None);
}

#[test]
fn expands_the_full_user_known_hosts_file_token_context() {
    let f = Fixture::new();
    let local_hostname = f.env.hostname.clone();
    let connection_hash = hex::encode(Sha1::digest(
        format!("{local_hostname}server.example.com2200remotejump").as_bytes(),
    ));
    let doc = f.doc(&lines(&[
        "Host alias",
        "  HostName server.example.com",
        "  Port 2200",
        "  User remote",
        "  ProxyJump jump",
        "  UserKnownHostsFile ~/.ssh/known_hosts-%C-%d-%h-%i-%j-%k-%L-%l-%n-%p-%r-%u-%%",
    ]));
    let name = [
        "known_hosts".to_string(),
        connection_hash,
        path_string(&f.env.home),
        "server.example.com".into(),
        f.env.uid.to_string(),
        "jump".into(),
        "alias".into(),
        "laptop".into(),
        local_hostname,
        "alias".into(),
        "2200".into(),
        "remote".into(),
        f.env.username.clone(),
        "%".into(),
    ]
    .join("-");
    assert_eq!(
        resolve_host(&doc, "alias", None).user_known_hosts_files,
        Some(vec![format!("{}/{}", f.home(&[".ssh"]), name)])
    );
}

#[test]
fn reads_server_alive_count_max_and_the_raw_algorithm_lists() {
    let f = Fixture::new();
    let doc = f.doc(&lines(&[
        "Host console",
        "  ServerAliveCountMax 5",
        "  Ciphers aes128-cbc",
        "  KexAlgorithms +diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1",
        "  HostKeyAlgorithms +ssh-rsa",
        "  MACs hmac-sha1",
    ]));
    let r = resolve_host(&doc, "console", None);
    assert_eq!(r.server_alive_count_max, Some(5.0));
    assert_eq!(r.ciphers.as_deref(), Some("aes128-cbc"));
    assert_eq!(
        r.kex_algorithms.as_deref(),
        Some("+diffie-hellman-group14-sha1,diffie-hellman-group-exchange-sha1")
    );
    assert_eq!(r.host_key_algorithms.as_deref(), Some("+ssh-rsa"));
    assert_eq!(r.macs.as_deref(), Some("hmac-sha1"));
}

#[test]
fn follows_include_files_with_evaluation_order_intact() {
    let f = Fixture::new();
    let inc = f.write_named(&lines(&["Host from-include", "  User inc"]), "conf.d/extra");
    let doc = f.doc(&lines(&[
        &format!("Include {}", path_string(&inc)),
        "",
        "Host base",
        "  User base",
    ]));
    let mut aliases: Vec<String> = list_hosts(&doc, None)
        .into_iter()
        .map(|h| h.alias)
        .collect();
    aliases.sort();
    assert_eq!(aliases, s(&["base", "from-include"]));
    assert_eq!(doc.file_order.len(), 2);
}

#[test]
fn records_an_error_for_unreadable_includes_but_keeps_parsing() {
    let f = Fixture::new();
    let missing = f.dir.path().join("nope").join("missing");
    let doc = f.doc(&lines(&[
        &format!("Include {}", path_string(&missing)),
        "Host still-here",
    ]));
    assert!(doc.error.as_deref().unwrap().contains("could not read"));
    let aliases: Vec<String> = list_hosts(&doc, None)
        .into_iter()
        .map(|h| h.alias)
        .collect();
    assert_eq!(aliases, s(&["still-here"]));
}

#[test]
fn expands_relative_and_globbed_includes_against_the_root_directory() {
    let f = Fixture::new();
    f.write_named("Host b-host\n", "conf.d/b.conf");
    f.write_named("Host a-host\n", "conf.d/a.conf");
    f.write_named("Host skipped\n", "conf.d/readme.txt");
    let doc = f.doc("Include conf.d/*.conf\n");
    let aliases: Vec<String> = list_hosts(&doc, None)
        .into_iter()
        .map(|h| h.alias)
        .collect();
    assert_eq!(aliases, s(&["a-host", "b-host"]));
}

// --- sessionEnvironment ---

fn map(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect()
}

#[test]
fn sends_variables_matched_by_send_env_honoring_later_removals() {
    let source = [
        ("LANG", "en_US.UTF-8"),
        ("LC_ALL", "C"),
        ("LC_TIME", "de_DE"),
        ("PATH", "/bin"),
    ];
    let env = session_environment(&BTreeMap::new(), &s(&["LANG", "LC_*", "-LC_ALL"]), source);
    assert_eq!(
        env,
        Some(map(&[("LANG", "en_US.UTF-8"), ("LC_TIME", "de_DE")]))
    );
}

#[test]
fn lets_set_env_override_sent_variables_and_returns_none_when_empty() {
    assert_eq!(
        session_environment(&map(&[("FOO", "bar")]), &s(&["FOO"]), [("FOO", "host")]),
        Some(map(&[("FOO", "bar")]))
    );
    assert_eq!(
        session_environment(&BTreeMap::new(), &[], [("FOO", "x")]),
        None
    );
}

// --- pattern + spec helpers ---

#[test]
fn matches_ssh_glob_patterns() {
    assert!(host_patterns_match(&["*.example.com"], "a.example.com"));
    assert!(host_patterns_match(&["prod-?"], "prod-1"));
    assert!(!host_patterns_match(&["prod-?"], "prod-12"));
    assert!(!host_patterns_match(&["*", "!secret"], "secret"));
}

#[test]
fn parses_host_specs() {
    let spec = |host: &str, user: Option<&str>, port: Option<u16>| HostSpec {
        host: host.to_string(),
        user: user.map(str::to_string),
        port,
    };
    assert_eq!(parse_host_spec("web"), spec("web", None, None));
    assert_eq!(parse_host_spec("root@web"), spec("web", Some("root"), None));
    assert_eq!(
        parse_host_spec("root@web:2222"),
        spec("web", Some("root"), Some(2222))
    );
    assert_eq!(parse_host_spec("[::1]:2200"), spec("::1", None, Some(2200)));
    assert_eq!(parse_host_spec("fe80::1"), spec("fe80::1", None, None));
}

#[test]
fn parses_proxy_jump_lists() {
    assert_eq!(
        parse_proxy_jump_list(Some("a, b@c:22 ,d")),
        s(&["a", "b@c:22", "d"])
    );
    assert!(parse_proxy_jump_list(Some("none")).is_empty());
    assert!(parse_proxy_jump_list(None).is_empty());
}

// --- host resolution at scale ---

#[test]
fn keeps_config_order_across_named_wildcard_negated_and_top_level_entries() {
    let f = Fixture::new();
    let doc = f.doc(
        "User top
Host web-1
  User alice
Host web-*
  User bob
  Port 2201
  IdentityFile /keys/web
Host web-1 !web-2
  Port 2202
Host !web-1
  User nobody
Host *
  IdentityFile /keys/all
Host web-2 db
  IdentityFile /keys/named
Host other
  User carol
",
    );
    let web1 = resolve_host(&doc, "web-1", None);
    // The top-level User comes first and wins; later blocks only fill gaps.
    assert_eq!(web1.user.as_deref(), Some("top"));
    assert_eq!(web1.port, 2201);
    assert_eq!(web1.identity_files, s(&["/keys/web", "/keys/all"]));
    let web2 = resolve_host(&doc, "web-2", None);
    assert_eq!(web2.user.as_deref(), Some("top"));
    assert_eq!(web2.port, 2201);
    // Named blocks interleave with wildcard blocks in file order.
    assert_eq!(
        web2.identity_files,
        s(&["/keys/web", "/keys/all", "/keys/named"])
    );
    assert_eq!(
        resolve_host(&doc, "db", None).identity_files,
        s(&["/keys/all", "/keys/named"])
    );
    // A negation-only block never matches, and neither does another host's block.
    let unknown = resolve_host(&doc, "unknown", None);
    assert_eq!(unknown.user.as_deref(), Some("top"));
    assert_eq!(unknown.port, 22);
}

#[test]
fn matches_concrete_names_exactly_like_the_glob_they_are() {
    let f = Fixture::new();
    let doc = f.doc("Host web.internal\n  User dotted\nHost WEB\n  User upper\n");
    assert_eq!(
        resolve_host(&doc, "web.internal", None).user.as_deref(),
        Some("dotted")
    );
    assert_eq!(resolve_host(&doc, "webxinternal", None).user, None);
    assert_eq!(resolve_host(&doc, "web", None).user, None);
    assert_eq!(
        resolve_host(&doc, "WEB", None).user.as_deref(),
        Some("upper")
    );
}

#[test]
fn lists_thousands_of_hosts_without_walking_the_config_once_per_host() {
    let f = Fixture::new();
    let count = 4000;
    let mut text = String::new();
    for i in 0..count {
        text.push_str(&format!(
            "Host node-{i}\n  HostName 10.0.{}.{}\n",
            i / 256,
            i % 256
        ));
    }
    text.push_str("Host node-1*\n  Port 2222\nHost *\n  User ops\n");
    let doc = f.doc(&text);
    let started = std::time::Instant::now();
    let hosts = list_hosts(&doc, None);
    let elapsed = started.elapsed();
    assert_eq!(hosts.len(), count);
    assert_eq!(hosts[12].resolved.hostname, "10.0.0.12");
    assert_eq!(hosts[12].resolved.port, 2222);
    assert_eq!(hosts[12].resolved.user.as_deref(), Some("ops"));
    assert_eq!(hosts[20].resolved.hostname, "10.0.0.20");
    assert_eq!(hosts[20].resolved.port, 22);
    assert_eq!(hosts[20].resolved.user.as_deref(), Some("ops"));
    // Linear work is a few tens of ms; the per-host walk it replaces took seconds at this size.
    assert!(elapsed.as_millis() < 1_000, "took {elapsed:?}");
}

#[test]
fn glob_matching_handles_backtracking() {
    assert!(glob_match("a*b*c", "axxbyyc"));
    assert!(!glob_match("a*b*c", "axxbyy"));
    assert!(glob_match("*", ""));
    assert!(glob_match("*.lab", "x.y.lab"));
    let _ = Path::new("/");
}
