//! The demo hosts' shells, run locally: what `hack/demo-env.mjs` serves over
//! its loopback sshds, without the SSH. Each demo host gets a scratch
//! "remote" home with the same files, prompt and helper commands
//! (`status`, `tailaccess`), and its shell is `bash --noprofile --rcfile
//! <home>/.muxus-demo-rc -i` with the same environment, so a terminal shows
//! byte for byte what the documentation screenshots show.
//!
//! [`DemoSource`] pairs these shells with the demo catalog. It is a
//! development aid for screenshots (`MUXUS_SCENE`), never the real backend.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use muxus_core::protocol::{LocalProfile, TerminalClientMessage, TerminalProfile};
use muxus_local::LocalEnvironment;

use crate::data_source::{DataSource, LoginEnvironment, connect_size, open_local_shell};
use crate::model::Catalog;
use crate::model::demo::demo_catalog;
use crate::session::{BoxFuture, SessionLink};

/// `DEMO_HOSTS`: alias, user and prompt.
const DEMO_SHELL_HOSTS: [(&str, &str, &str); 8] = [
    ("web-01", "deploy", "web-01"),
    ("web-02", "deploy", "web-02"),
    ("cache-01", "deploy", "cache-01"),
    ("bastion", "jump", "bastion"),
    ("db-01", "postgres", "db-01"),
    ("build-01", "ci", "build-01"),
    ("lab-leaf-01", "admin", "leaf-01"),
    ("lab-spine-01", "admin", "spine-01"),
];

/// `SHELL_RC` with `__USER__` and `__HOST__` filled in.
fn shell_rc(user: &str, host: &str) -> String {
    format!(
        r#"
export PATH="$HOME/bin:$PATH"
export PS1='\[\e[38;5;114m\]{user}@{host}\[\e[0m\]:\[\e[38;5;75m\]\w\[\e[0m\]\$ '
alias ls='ls --color=auto'
# The scratch files belong to whoever runs the capture; screenshots should not
# say so, and the demo host has its own user anyway.
ll() {{
  command ls -lh --color=auto "$@" | sed -E "s/ $(id -un) +$(id -gn) / {user} {user} /"
}}
printf '\e[2J\e[H'
"#
    )
}

/// `demoCommands`: the helpers the screenshots run.
fn demo_commands(prompt: &str) -> [(&'static str, String); 2] {
    [
        (
            "status",
            format!(
                r#"#!/usr/bin/env bash
printf '\e[1m%-18s %-9s %-8s %s\e[0m\n' UNIT STATE CPU MEMORY
printf '%-18s \e[32m%-9s\e[0m %-8s %s\n' edge.service active 3.1% 218M
printf '%-18s \e[32m%-9s\e[0m %-8s %s\n' nginx.service active 0.4% 24M
printf '%-18s \e[32m%-9s\e[0m %-8s %s\n' node-exporter active 0.2% 18M
printf '%-18s \e[33m%-9s\e[0m %-8s %s\n' backup.timer waiting - -
echo
printf 'host      \e[1m{prompt}\e[0m   uptime \e[1m12 days\e[0m   load \e[1m0.31 0.28 0.22\e[0m\n'
"#
            ),
        ),
        (
            "tailaccess",
            "#!/usr/bin/env bash\ntail -n 12 \"$HOME/logs/access.log\"\n".to_string(),
        ),
    ]
}

/// `REMOTE_TREE['web-01']`, or the default tree of the other hosts.
fn remote_tree(alias: &str, prompt: &str) -> Vec<(String, String)> {
    if alias != "web-01" {
        return vec![
            (
                "README.md".into(),
                format!("# {prompt}\n\nDemo host used by the Muxus documentation screenshots.\n"),
            ),
            ("notes.txt".into(), "nothing to see here\n".into()),
            (
                "src/main.go".into(),
                "package main\n\nfunc main() {}\n".into(),
            ),
            ("logs/access.log".into(), "demo\n".into()),
        ];
    }
    let access_log: Vec<String> = (0..40)
        .map(|i| {
            format!(
                "10.20.{}.{} - - [12/Mar/2026:09:{:02}:11] \"GET /healthz HTTP/1.1\" 200 12",
                i % 5,
                i + 3,
                i % 60
            )
        })
        .collect();
    vec![
        (
            "docker-compose.yml".into(),
            r#"services:
  edge:
    image: ghcr.io/acme/edge:2.8.1
    restart: unless-stopped
    ports:
      - "80:8080"
    environment:
      LOG_LEVEL: info
      UPSTREAM: http://cache-01:6379
    healthcheck:
      test: ["CMD", "curl", "-fsS", "http://localhost:8080/healthz"]
      interval: 10s
"#
            .into(),
        ),
        (
            "deploy.sh".into(),
            r#"#!/usr/bin/env bash
set -euo pipefail

RELEASE="${1:-latest}"
echo "==> pulling edge:${RELEASE}"
docker compose pull edge
echo "==> restarting"
docker compose up -d --no-deps edge
docker compose ps
"#
            .into(),
        ),
        (
            "README.md".into(),
            r#"# web-01

Front-end node behind the load balancer.

- `deploy.sh <tag>` rolls a new edge release
- `docker compose logs -f edge` tails the access log
- metrics: http://web-01:9100/metrics
"#
            .into(),
        ),
        (
            "conf/nginx.conf".into(),
            r#"worker_processes auto;

events { worker_connections 4096; }

http {
    access_log /var/log/nginx/access.log combined;
    sendfile on;

    upstream edge {
        server 127.0.0.1:8080 max_fails=3 fail_timeout=15s;
    }

    server {
        listen 80;
        server_name web-01;

        location / {
            proxy_pass http://edge;
            proxy_set_header X-Forwarded-For $proxy_add_x_forwarded_for;
        }
    }
}
"#
            .into(),
        ),
        (
            "conf/edge.env".into(),
            "LOG_LEVEL=info\nUPSTREAM=http://cache-01:6379\nTIMEOUT=5s\n".into(),
        ),
        ("logs/access.log".into(), access_log.join("\n")),
        (
            "logs/error.log".into(),
            "no errors in the last 24h\n".into(),
        ),
    ]
}

/// The moment the documentation's demo files carry (Jul 25 20:17, shown in
/// UTC by the demo shell), so `ll` lists the same dates.
fn demo_mtime() -> SystemTime {
    // 2026-07-25T20:17:00Z
    SystemTime::UNIX_EPOCH + Duration::from_secs(1_785_010_620)
}

fn write_file(path: &Path, content: &str, executable: bool) -> io::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    #[cfg(unix)]
    if executable {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    }
    #[cfg(not(unix))]
    let _ = executable;
    Ok(())
}

/// Every directory under `root`, deepest first, then `root` itself.
fn directories(root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                out.extend(directories(&path));
            }
        }
    }
    out.push(root.to_path_buf());
    out
}

/// Build one host's scratch home (`buildHome` for one host) and return its root.
pub fn build_demo_home(base: &Path, alias: &str) -> io::Result<Option<PathBuf>> {
    let Some((_, user, prompt)) = DEMO_SHELL_HOSTS
        .iter()
        .find(|(candidate, _, _)| *candidate == alias)
    else {
        return Ok(None);
    };
    let root = base.join(alias);
    let rc = shell_rc(user, prompt);
    for (name, content) in remote_tree(alias, prompt) {
        write_file(&root.join(name), &content, false)?;
    }
    if alias == "web-01" {
        fs::create_dir_all(root.join("backups"))?;
    }
    write_file(&root.join(".muxus-demo-rc"), &rc, false)?;
    write_file(&root.join(".bash_profile"), &rc, false)?;
    write_file(&root.join(".sudo_as_admin_successful"), "", false)?;
    for (name, script) in demo_commands(prompt) {
        write_file(&root.join("bin").join(name), &script, true)?;
    }
    // The listing the screenshots show carries the capture's dates.
    let mtime = demo_mtime();
    for dir in directories(&root) {
        if let Ok(entries) = fs::read_dir(&dir) {
            for entry in entries.flatten() {
                if let Ok(file) = fs::File::options().write(true).open(entry.path()) {
                    let _ = file.set_modified(mtime);
                }
            }
        }
    }
    for dir in directories(&root) {
        if let Ok(file) = fs::File::open(&dir) {
            let _ = file.set_modified(mtime);
        }
    }
    Ok(Some(root))
}

/// The profile and environment that start a demo host's shell in `root`.
pub fn demo_shell(root: &Path, prompt: &str) -> (LocalProfile, LocalEnvironment) {
    let rc = root.join(".muxus-demo-rc").display().to_string();
    let profile = LocalProfile {
        shell: Some("/usr/bin/bash".into()),
        args: Some(vec![
            "--noprofile".into(),
            "--rcfile".into(),
            rc,
            "-i".into(),
        ]),
        cwd: Some(root.display().to_string()),
        startup_command: None,
    };
    let vars: HashMap<String, String> = [
        ("PATH", "/usr/local/bin:/usr/bin:/bin"),
        ("TERM", "xterm-256color"),
        ("LANG", "C.UTF-8"),
        ("SHELL", "/usr/bin/bash"),
        ("HOSTNAME", prompt),
        ("MUXUS_DEMO", "1"),
        ("TZ", "UTC"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .chain([("HOME".to_string(), root.display().to_string())])
    .collect();
    let environment = LocalEnvironment {
        vars,
        integration_root: None,
        platform: muxus_local::Platform::current(),
    };
    (profile, environment)
}

/// The demo catalog, with each demo host's shell run locally.
pub struct DemoSource {
    base: PathBuf,
    login: LoginEnvironment,
    catalog: Catalog,
}

impl DemoSource {
    /// `base` holds the demo homes (a scratch directory).
    pub fn new(base: PathBuf, login: LoginEnvironment) -> Self {
        Self {
            base,
            login,
            catalog: demo_catalog(),
        }
    }

    /// Serve `catalog` instead of the plain demo catalog.
    pub fn with_catalog(mut self, catalog: Catalog) -> Self {
        self.catalog = catalog;
        self
    }

    fn local_environment(&self) -> LocalEnvironment {
        LocalEnvironment {
            vars: self.login.vars(),
            ..LocalEnvironment::current()
        }
    }
}

impl DataSource for DemoSource {
    fn load_catalog(&self) -> BoxFuture<'static, Result<Catalog, String>> {
        let catalog = self.catalog.clone();
        Box::pin(async move { Ok(catalog) })
    }

    fn open_terminal(&self, connect: TerminalClientMessage) -> SessionLink {
        let size = connect_size(&connect);
        let TerminalClientMessage::Connect { profile, .. } = connect else {
            return SessionLink::failed("This session cannot be reattached.");
        };
        match profile {
            TerminalProfile::Local(profile) => {
                open_local_shell(&profile, size, &self.local_environment())
            }
            TerminalProfile::Ssh(ssh) => {
                let prompt = DEMO_SHELL_HOSTS
                    .iter()
                    .find(|(alias, _, _)| *alias == ssh.target)
                    .map(|(_, _, prompt)| *prompt);
                match (prompt, build_demo_home(&self.base, &ssh.target)) {
                    (Some(prompt), Ok(Some(root))) => {
                        let (profile, environment) = demo_shell(&root, prompt);
                        open_local_shell(&profile, size, &environment)
                    }
                    (_, Err(err)) => {
                        SessionLink::failed(format!("Could not prepare the demo host: {err}"))
                    }
                    _ => {
                        SessionLink::failed(format!("{} is not one of the demo hosts.", ssh.target))
                    }
                }
            }
            _ => SessionLink::failed("The demo serves SSH hosts and local shells only."),
        }
    }

    fn supports(&self, profile: &TerminalProfile) -> bool {
        matches!(profile, TerminalProfile::Local(_) | TerminalProfile::Ssh(_))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_the_web_01_home_the_screenshots_list() {
        let base = tempfile::tempdir().unwrap();
        let root = build_demo_home(base.path(), "web-01").unwrap().unwrap();
        for name in [
            "README.md",
            "deploy.sh",
            "docker-compose.yml",
            "conf/nginx.conf",
            "conf/edge.env",
            "logs/access.log",
            "bin/status",
            ".muxus-demo-rc",
        ] {
            assert!(root.join(name).is_file(), "{name}");
        }
        assert!(root.join("backups").is_dir());
        // The sizes `ll` shows in the documentation.
        assert_eq!(fs::metadata(root.join("README.md")).unwrap().len(), 188);
        assert_eq!(fs::metadata(root.join("deploy.sh")).unwrap().len(), 198);
        assert_eq!(
            fs::metadata(root.join("docker-compose.yml")).unwrap().len(),
            292
        );
        assert_eq!(
            fs::metadata(root.join("README.md"))
                .unwrap()
                .modified()
                .unwrap(),
            demo_mtime()
        );
        assert_eq!(
            fs::metadata(root.join("conf/nginx.conf")).unwrap().len(),
            433
        );
        assert!(build_demo_home(base.path(), "unknown").unwrap().is_none());
    }

    #[test]
    fn fills_the_prompt_into_the_shell_rc() {
        let rc = shell_rc("deploy", "web-01");
        assert!(rc.contains(r"\]deploy@web-01\["));
        assert!(rc.contains("/ deploy deploy /"));
    }
}
