//! Desktop command-line arguments (port of `electron/src/command-line.ts`):
//! `--host`, `--folder`, `--workspace`, `--connect` with `--user`/`--port`,
//! and ssh:// or telnet:// links handed over by the system URL handler.

use muxus_core::api::{AppWindowLaunch, CommandLineLaunch};
use muxus_core::connection_links::{
    CONNECTION_LINK_MAX_LENGTH, is_ipv6_address, is_ssh_hostname, is_ssh_user,
    looks_like_connection_link,
};
use muxus_core::js;

const MAX_TARGET_LENGTH: usize = 500;
// One character past the limit is kept so the window can say the link is too long.
const MAX_LINK_ARGUMENT_LENGTH: usize = CONNECTION_LINK_MAX_LENGTH + 1;

/// Parts of a `--connect` target that may also be given as separate flags.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ConnectOptions {
    pub user: Option<String>,
    pub port: Option<String>,
}

#[derive(Clone, Copy)]
enum TargetFlag {
    Host,
    Folder,
    Workspace,
    Connect,
}

fn target_flag(flag: &str) -> Option<TargetFlag> {
    match flag {
        "--host" => Some(TargetFlag::Host),
        "--folder" => Some(TargetFlag::Folder),
        "--workspace" => Some(TargetFlag::Workspace),
        "--connect" => Some(TargetFlag::Connect),
        _ => None,
    }
}

#[derive(Clone, Copy)]
enum ConnectFlag {
    User,
    Port,
}

fn connect_flag(flag: &str) -> Option<ConnectFlag> {
    match flag {
        "--user" => Some(ConnectFlag::User),
        "--port" => Some(ConnectFlag::Port),
        _ => None,
    }
}

fn valid_port(value: &str) -> Option<String> {
    if value.is_empty() || value.len() > 5 || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let port: u32 = value.parse().ok()?;
    (1..=65_535).contains(&port).then(|| port.to_string())
}

/// `/^\[([^\]]+)\](?::([^:]*))?$/`.
fn bracketed_address(address: &str) -> Option<(&str, Option<&str>)> {
    let inner_end = address.strip_prefix('[')?.find(']')? + 1;
    let host = &address[1..inner_end];
    if host.is_empty() {
        return None;
    }
    let rest = &address[inner_end + 1..];
    if rest.is_empty() {
        return Some((host, None));
    }
    let port = rest.strip_prefix(':')?;
    (!port.contains(':')).then_some((host, Some(port)))
}

/// Validate an ad-hoc `[user@]host[:port]` SSH target and fold in a separately
/// supplied user or port. A part given both ways is rejected rather than
/// guessed. IPv6 hosts may be bracketed and are bracketed again when a port
/// follows them.
pub fn parse_connect_target(target: &str, options: &ConnectOptions) -> Option<String> {
    let at = target.rfind('@');
    if at == Some(0) {
        return None;
    }
    let target_user = at.map(|at| &target[..at]);
    let address = at.map_or(target, |at| &target[at + 1..]);

    let host: &str;
    let target_port: Option<&str>;
    if let Some((bracketed_host, port)) = bracketed_address(address) {
        host = bracketed_host;
        if !is_ipv6_address(host) && !is_ssh_hostname(host) {
            return None;
        }
        target_port = port;
    } else if address.find(':') != address.rfind(':') {
        // Unbracketed IPv6 cannot carry a port, matching how the server reads it.
        host = address;
        if !is_ipv6_address(host) {
            return None;
        }
        target_port = None;
    } else {
        match address.find(':') {
            Some(colon) => {
                host = &address[..colon];
                target_port = Some(&address[colon + 1..]);
            }
            None => {
                host = address;
                target_port = None;
            }
        }
        if !is_ssh_hostname(host) {
            return None;
        }
    }

    if target_user.is_some() && options.user.is_some() {
        return None;
    }
    if target_port.is_some() && options.port.is_some() {
        return None;
    }
    let user = target_user.or(options.user.as_deref());
    let raw_port = target_port.or(options.port.as_deref());
    if user.is_some_and(|user| !is_ssh_user(user)) {
        return None;
    }
    let port = match raw_port {
        Some(raw) => Some(valid_port(raw)?),
        None => None,
    };

    let host_part = if port.is_some() && host.contains(':') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    let normalized = format!(
        "{}{host_part}{}",
        user.filter(|user| !user.is_empty())
            .map(|user| format!("{user}@"))
            .unwrap_or_default(),
        port.map(|port| format!(":{port}")).unwrap_or_default()
    );
    (js::utf16_len(&normalized) <= MAX_TARGET_LENGTH).then_some(normalized)
}

/// An ssh:// or telnet:// link as a launch request. It stays unparsed here so
/// the window that receives it can report a malformed one.
pub fn link_launch(url: &str) -> Option<CommandLineLaunch> {
    looks_like_connection_link(url).then(|| CommandLineLaunch::Url {
        name: js::utf16_prefix(url, MAX_LINK_ARGUMENT_LENGTH).to_owned(),
    })
}

/// Parse exactly one desktop launch target from the full argv array
/// (the executable first).
pub fn parse_command_line_launch<S: AsRef<str>>(argv: &[S]) -> Option<CommandLineLaunch> {
    let mut launch: Option<CommandLineLaunch> = None;
    let mut connect_options = ConnectOptions::default();

    let mut index = 0;
    while index < argv.len() {
        let argument = argv[index].as_ref();
        // Linux and Windows hand a link to the executable as a plain argument.
        if index > 0
            && !argument.starts_with('-')
            && let Some(link) = link_launch(argument)
        {
            if launch.is_some() {
                return None;
            }
            launch = Some(link);
            index += 1;
            continue;
        }
        let separator = argument.find('=');
        let flag = separator.map_or(argument, |separator| &argument[..separator]);
        let kind = target_flag(flag);
        let option = connect_flag(flag);
        if kind.is_none() && option.is_none() {
            index += 1;
            continue;
        }

        let raw_value = match separator {
            None => {
                index += 1;
                argv.get(index).map(AsRef::as_ref)
            }
            Some(separator) => Some(&argument[separator + 1..]),
        };
        let value = raw_value.map(js::js_trim).unwrap_or("");
        if value.is_empty() || value.starts_with("--") || js::utf16_len(value) > MAX_TARGET_LENGTH {
            return None;
        }
        if let Some(option) = option {
            let slot = match option {
                ConnectFlag::User => &mut connect_options.user,
                ConnectFlag::Port => &mut connect_options.port,
            };
            if slot.is_some() {
                return None;
            }
            *slot = Some(value.to_owned());
            index += 1;
            continue;
        }
        if launch.is_some() {
            return None;
        }
        let name = value.to_owned();
        launch = Some(match kind.expect("a target flag") {
            TargetFlag::Host => CommandLineLaunch::Host { name },
            TargetFlag::Folder => CommandLineLaunch::Folder { name },
            TargetFlag::Workspace => CommandLineLaunch::Workspace { name },
            TargetFlag::Connect => CommandLineLaunch::Connect { name },
        });
        index += 1;
    }

    match launch {
        Some(CommandLineLaunch::Connect { name }) => parse_connect_target(&name, &connect_options)
            .map(|name| CommandLineLaunch::Connect { name }),
        // --user and --port only describe a --connect target.
        other => (connect_options == ConnectOptions::default())
            .then_some(other)
            .flatten(),
    }
}

/// Validate the structured payload forwarded by a second instance (or any
/// other untrusted sender).
pub fn parse_command_line_launch_data(value: &serde_json::Value) -> Option<CommandLineLaunch> {
    let candidate = value.as_object()?;
    let kind = candidate.get("kind")?.as_str()?;
    if !matches!(kind, "host" | "folder" | "workspace" | "connect" | "url") {
        return None;
    }
    let name = candidate.get("name")?.as_str()?;
    if kind == "url" {
        return link_launch(name);
    }
    let name = if kind == "connect" {
        parse_connect_target(js::js_trim(name), &ConnectOptions::default())?
    } else {
        js::js_trim(name).to_owned()
    };
    if name.is_empty() || js::utf16_len(&name) > MAX_TARGET_LENGTH {
        return None;
    }
    Some(match kind {
        "host" => CommandLineLaunch::Host { name },
        "folder" => CommandLineLaunch::Folder { name },
        "workspace" => CommandLineLaunch::Workspace { name },
        _ => CommandLineLaunch::Connect { name },
    })
}

/// SFTP and settings windows do not handle command-line requests.
pub fn can_handle_command_line_launch(window_launch: Option<&AppWindowLaunch>) -> bool {
    !matches!(
        window_launch,
        Some(AppWindowLaunch::Sftp { .. } | AppWindowLaunch::Settings { .. })
    )
}
