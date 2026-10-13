//! ssh:// and telnet:// links handed to the desktop app by a browser or
//! another program (port of `shared/src/connection-links.ts`). ssh:// follows
//! draft-ietf-secsh-scp-sftp-ssh-uri, `ssh://[user[;fingerprint=…]@]host[:port][/]`,
//! and telnet:// follows RFC 4248, `telnet://[user@]host[:port][/]`.
//!
//! Lengths and the quoted excerpts in error messages count UTF-16 code units,
//! like the TypeScript, so both builds accept and reject the same links with
//! the same words.

use std::sync::LazyLock;

use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;

use crate::js;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ConnectionLinkScheme {
    Ssh,
    Telnet,
}

impl ConnectionLinkScheme {
    pub fn as_str(self) -> &'static str {
        match self {
            ConnectionLinkScheme::Ssh => "ssh",
            ConnectionLinkScheme::Telnet => "telnet",
        }
    }
}

pub const CONNECTION_LINK_SCHEMES: [ConnectionLinkScheme; 2] =
    [ConnectionLinkScheme::Ssh, ConnectionLinkScheme::Telnet];

/// A parsed and validated link.
#[skip_serializing_none]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionLink {
    pub scheme: ConnectionLinkScheme,
    /// Percent-decoded; an IPv6 literal without its brackets.
    pub host: String,
    /// Absent when the link leaves the default port to the host's settings.
    pub port: Option<u16>,
    /// Telnet has no login of its own, so a telnet:// user is only advisory.
    pub user: Option<String>,
    /// ssh:// only: the host key the server must present, in canonical form.
    pub host_key_fingerprint: Option<String>,
}

/// Why a link was refused; the message is shown to the user as is.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct ConnectionLinkError(pub String);

/// `{ ok: true, link } | { ok: false, error }` in the TypeScript.
pub type ConnectionLinkResult = Result<ConnectionLink, ConnectionLinkError>;

pub const CONNECTION_LINK_MAX_LENGTH: usize = 2048;

const MAX_HOST_LENGTH: usize = 253;
const MAX_USER_LENGTH: usize = 255;

// Hosts and users can reach a ProxyCommand shell through %h and %r, so like
// OpenSSH for command-line hosts only plain names are accepted from outside.

/// `SSH_USER_PATTERN`: `/^(?!-)[\w.@+-]+$/`.
pub fn is_ssh_user(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with('-')
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b'@' | b'+' | b'-'))
}

/// `SSH_HOSTNAME_PATTERN`: `/^(?![-.])[A-Za-z0-9._-]+$/`.
pub fn is_ssh_hostname(value: &str) -> bool {
    !value.is_empty()
        && !value.starts_with(['-', '.'])
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

static IPV6_ADDRESS_PATTERN: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*:[0-9A-Fa-f:.]*(?:%[A-Za-z0-9_.-]+)?$")
        .expect("valid regex")
});

/// `IPV6_ADDRESS_PATTERN`: an IPv6 literal, optionally with a `%zone`.
pub fn is_ipv6_address(value: &str) -> bool {
    IPV6_ADDRESS_PATTERN.is_match(value)
}

// `(?i-u)`: JavaScript's non-Unicode `/i` folds ASCII letters only.
static SHA256_FINGERPRINT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i-u)^SHA256:([A-Za-z0-9+/_-]{43})=?$").expect("valid regex"));
static MD5_FINGERPRINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)^MD5:((?:[0-9a-f]{2}:){15}[0-9a-f]{2})$").expect("valid regex")
});
/// The draft's own form: key type, then the MD5 digest in dash-separated pairs.
static DRAFT_FINGERPRINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)^([a-z][a-z0-9.@-]*?)-((?:[0-9a-f]{2}-){15}[0-9a-f]{2})$")
        .expect("valid regex")
});
static TYPED_MD5_FINGERPRINT: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i-u)^([a-z][a-z0-9.@-]*) (MD5:(?:[0-9a-f]{2}:){15}[0-9a-f]{2})$")
        .expect("valid regex")
});

/// Canonical host key fingerprint, or `None` when the value is not one:
/// `SHA256:<base64>` as `ssh-keygen -l` prints it (URL-safe base64 is
/// accepted too), `MD5:<hex pairs>`, or the draft's `<key type>-<hex pairs>`,
/// which keeps its key type as `<key type> MD5:<hex pairs>`, a form that is
/// accepted in turn.
pub fn normalize_host_key_fingerprint(value: &str) -> Option<String> {
    if let Some(sha256) = SHA256_FINGERPRINT.captures(value) {
        return Some(format!(
            "SHA256:{}",
            sha256[1].replace('-', "+").replace('_', "/")
        ));
    }
    if let Some(md5) = MD5_FINGERPRINT.captures(value) {
        return Some(format!("MD5:{}", md5[1].to_lowercase()));
    }
    if let Some(draft) = DRAFT_FINGERPRINT.captures(value) {
        return Some(format!(
            "{} MD5:{}",
            draft[1].to_lowercase(),
            draft[2].to_lowercase().replace('-', ":")
        ));
    }
    if let Some(typed) = TYPED_MD5_FINGERPRINT.captures(value) {
        return Some(format!(
            "{} MD5:{}",
            typed[1].to_lowercase(),
            typed[2][4..].to_lowercase()
        ));
    }
    None
}

/// The URL scheme at the start of `value` (`/^[A-Za-z][A-Za-z0-9+.-]*:/`),
/// without its colon.
fn scheme_prefix(value: &str) -> Option<&str> {
    let bytes = value.as_bytes();
    if !bytes.first().is_some_and(u8::is_ascii_alphabetic) {
        return None;
    }
    let end = bytes
        .iter()
        .position(|b| !(b.is_ascii_alphanumeric() || matches!(b, b'+' | b'.' | b'-')))?;
    (bytes[end] == b':').then(|| &value[..end])
}

/// Whether a command-line argument is meant as a link rather than a file or switch.
pub fn looks_like_connection_link(argument: &str) -> bool {
    if let Some(scheme) = scheme_prefix(argument)
        && argument[scheme.len() + 1..].starts_with("//")
    {
        return true;
    }
    let lower = argument.get(..7).map(str::to_ascii_lowercase);
    argument
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("ssh:"))
        || lower.as_deref() == Some("telnet:")
}

/// `decodeURIComponent`, refusing a stray `%` and invalid UTF-8 the way it throws.
fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    for (index, &byte) in bytes.iter().enumerate() {
        if byte == b'%'
            && !(bytes.get(index + 1).is_some_and(u8::is_ascii_hexdigit)
                && bytes.get(index + 2).is_some_and(u8::is_ascii_hexdigit))
        {
            return None;
        }
    }
    percent_encoding::percent_decode_str(value)
        .decode_utf8()
        .ok()
        .map(|decoded| decoded.into_owned())
}

fn quoted(value: &str) -> String {
    if js::utf16_len(value) > 64 {
        format!("“{}…”", js::utf16_prefix(value, 63))
    } else {
        format!("“{value}”")
    }
}

fn failure(error: impl Into<String>) -> ConnectionLinkResult {
    Err(ConnectionLinkError(error.into()))
}

/// Raw spaces, quotes and control characters never belong in a URL
/// (`/[\s"<>\\^`{|}\p{Cc}]/u`).
fn has_forbidden_url_character(url: &str) -> bool {
    url.chars().any(|c| {
        js::is_js_whitespace(c)
            || matches!(c, '"' | '<' | '>' | '\\' | '^' | '`' | '{' | '|' | '}')
            || c.is_control()
    })
}

/// `/^\[([^\]]*)\](?::(.*))?$/`: an IPv6 literal and an optional port.
fn bracketed_host(host_port: &str) -> Option<(&str, Option<&str>)> {
    let inner_end = host_port.strip_prefix('[')?.find(']')? + 1;
    let inner = &host_port[1..inner_end];
    let rest = &host_port[inner_end + 1..];
    if rest.is_empty() {
        Some((inner, None))
    } else {
        rest.strip_prefix(':').map(|port| (inner, Some(port)))
    }
}

/// Parse and validate one link. Nothing in it is trusted until this accepts it.
pub fn parse_connection_link(input: &str) -> ConnectionLinkResult {
    let url = js::js_trim(input);
    if js::utf16_len(url) > CONNECTION_LINK_MAX_LENGTH {
        return failure(format!(
            "The link is longer than {CONNECTION_LINK_MAX_LENGTH} characters."
        ));
    }
    let raw_scheme = scheme_prefix(url);
    let scheme = match raw_scheme.map(str::to_ascii_lowercase).as_deref() {
        Some("ssh") => ConnectionLinkScheme::Ssh,
        Some("telnet") => ConnectionLinkScheme::Telnet,
        _ => return failure("Muxus opens only ssh:// and telnet:// links."),
    };
    let scheme_length = raw_scheme.map_or(0, str::len) + 1;
    if !url[scheme_length..].starts_with("//") || has_forbidden_url_character(url) {
        return failure("The link is not a valid URL.");
    }

    let rest = &url[scheme_length + 2..];
    let hierarchy = match rest.find(['?', '#']) {
        Some(query_at) => &rest[..query_at],
        None => rest,
    };
    let mut authority_end = hierarchy.find('/');
    let last_at = hierarchy.rfind('@');
    // A base64 fingerprint may carry an unescaped "/" before the "@".
    if let (Some(end), Some(at)) = (authority_end, last_at)
        && at > end
        && hierarchy[..at]
            .to_ascii_lowercase()
            .contains(";fingerprint=")
    {
        authority_end = hierarchy[at..].find('/').map(|offset| at + offset);
    }
    // A path, query or fragment has no meaning for either scheme and is ignored.
    let authority = match authority_end {
        Some(end) => &hierarchy[..end],
        None => hierarchy,
    };

    let at = authority.rfind('@');
    let userinfo = at.map(|at| &authority[..at]);
    let host_port = match at {
        Some(at) => &authority[at + 1..],
        None => authority,
    };

    let mut user = None;
    let mut host_key_fingerprint: Option<String> = None;
    if let Some(userinfo) = userinfo {
        let separator = userinfo.find(';');
        let raw_user = match separator {
            Some(separator) => &userinfo[..separator],
            None => userinfo,
        };
        if separator.is_some() && scheme == ConnectionLinkScheme::Telnet {
            return failure("Telnet links take no connection parameters.");
        }
        if raw_user.contains(':') {
            return failure(
                "Links with a password are not accepted. Muxus asks for the password instead.",
            );
        }
        let Some(decoded_user) = percent_decode(raw_user) else {
            return failure("The link is not a valid URL.");
        };
        if !decoded_user.is_empty() {
            if js::utf16_len(&decoded_user) > MAX_USER_LENGTH || !is_ssh_user(&decoded_user) {
                return failure(format!(
                    "{} is not a valid user name.",
                    quoted(&decoded_user)
                ));
            }
            user = Some(decoded_user);
        }
        if let Some(separator) = separator {
            for parameter in userinfo[separator + 1..].split([';', ',']) {
                if parameter.is_empty() {
                    continue;
                }
                let equals = parameter.find('=');
                let name = match equals {
                    Some(equals) => &parameter[..equals],
                    None => parameter,
                };
                // Connection parameters this client does not know are ignored, as the draft asks.
                if name.to_lowercase() != "fingerprint" {
                    continue;
                }
                if host_key_fingerprint.is_some() {
                    return failure("The link names more than one fingerprint.");
                }
                let value = &parameter[equals.map_or(0, |equals| equals + 1)..];
                host_key_fingerprint =
                    percent_decode(value).and_then(|value| normalize_host_key_fingerprint(&value));
                if host_key_fingerprint.is_none() {
                    return failure(
                        "The fingerprint is not in the form SHA256:… that ssh-keygen -l prints.",
                    );
                }
            }
        }
    }

    let raw_host: &str;
    let raw_port: Option<&str>;
    let mut host: Option<String> = None;
    let bracketed = bracketed_host(host_port);
    if host_port.starts_with('[') && bracketed.is_none() {
        return failure(format!("{} is not a valid host name.", quoted(host_port)));
    }
    if let Some((inner, port)) = bracketed {
        raw_host = inner;
        raw_port = port;
        // RFC 6874 writes an IPv6 zone's "%" as "%25".
        let literal = raw_host.replace("%25", "%");
        if is_ipv6_address(&literal) {
            host = Some(literal);
        }
    } else {
        let colon = host_port.find(':');
        raw_host = match colon {
            Some(colon) => &host_port[..colon],
            None => host_port,
        };
        raw_port = colon.map(|colon| &host_port[colon + 1..]);
        if let Some(decoded) = percent_decode(raw_host)
            && is_ssh_hostname(&decoded)
        {
            host = Some(decoded);
        }
    }
    if raw_host.is_empty() {
        return failure("The link does not name a host.");
    }
    let host = match host {
        Some(host) if js::utf16_len(&host) <= MAX_HOST_LENGTH => host,
        _ => {
            let shown = percent_decode(raw_host).unwrap_or_else(|| raw_host.to_owned());
            return failure(format!("{} is not a valid host name.", quoted(&shown)));
        }
    };

    let mut port = None;
    // RFC 3986 allows an empty port, which means the default one.
    if let Some(raw_port) = raw_port.filter(|raw| !raw.is_empty()) {
        let parsed =
            if (1..=5).contains(&raw_port.len()) && raw_port.bytes().all(|b| b.is_ascii_digit()) {
                raw_port.parse::<u32>().unwrap_or(0)
            } else {
                0
            };
        if !(1..=65_535).contains(&parsed) {
            return failure("The port must be a number from 1 to 65535.");
        }
        port = Some(parsed as u16);
    }

    Ok(ConnectionLink {
        scheme,
        host,
        port,
        user,
        host_key_fingerprint,
    })
}

/// The link as an ad-hoc `[user@]host[:port]` SSH target, bracketing IPv6 before a port.
pub fn connection_link_target(link: &ConnectionLink) -> String {
    let host = if link.port.is_some() && link.host.contains(':') {
        format!("[{}]", link.host)
    } else {
        link.host.clone()
    };
    let user = link
        .user
        .as_deref()
        .filter(|user| !user.is_empty())
        .map(|user| format!("{user}@"))
        .unwrap_or_default();
    let port = link.port.map(|port| format!(":{port}")).unwrap_or_default();
    format!("{user}{host}{port}")
}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA256: &str = "SHA256:nThbg6kXUpJWGl7E1IGOCspRomTxdCARLviKw6E5SY8";
    const MD5_PAIRS: &str = "c1:b1:30:29:d7:b8:de:6c:97:77:10:d7:46:41:63:87";

    fn link(url: &str) -> ConnectionLink {
        parse_connection_link(url).unwrap_or_else(|error| panic!("rejected {url}: {error}"))
    }

    fn rejection(url: &str) -> String {
        match parse_connection_link(url) {
            Ok(_) => panic!("accepted {url}"),
            Err(error) => error.0,
        }
    }

    fn ssh(host: &str) -> ConnectionLink {
        ConnectionLink {
            scheme: ConnectionLinkScheme::Ssh,
            host: host.into(),
            port: None,
            user: None,
            host_key_fingerprint: None,
        }
    }

    fn telnet(host: &str) -> ConnectionLink {
        ConnectionLink {
            scheme: ConnectionLinkScheme::Telnet,
            ..ssh(host)
        }
    }

    /// `encodeURIComponent` for the characters these tests need.
    fn encode_uri_component(value: &str) -> String {
        value
            .replace(':', "%3A")
            .replace('/', "%2F")
            .replace('+', "%2B")
    }

    #[test]
    fn reads_user_host_and_port() {
        assert_eq!(
            link("ssh://admin@10.0.0.1:2222"),
            ConnectionLink {
                port: Some(2222),
                user: Some("admin".into()),
                ..ssh("10.0.0.1")
            }
        );
        assert_eq!(link("ssh://edge-router"), ssh("edge-router"));
        assert_eq!(
            link("SSH://Edge-Router.example.com/"),
            ssh("Edge-Router.example.com")
        );
    }

    #[test]
    fn leaves_the_port_to_the_host_settings_when_the_link_has_none() {
        assert_eq!(link("ssh://admin@host").port, None);
        assert_eq!(link("ssh://host:").port, None);
        assert_eq!(link("ssh://host:022").port, Some(22));
    }

    #[test]
    fn accepts_ipv6_literals_in_brackets_with_an_rfc_6874_zone() {
        assert_eq!(
            link("ssh://root@[2001:db8::1]:830"),
            ConnectionLink {
                port: Some(830),
                user: Some("root".into()),
                ..ssh("2001:db8::1")
            }
        );
        assert_eq!(link("ssh://[fe80::1%25eth0]").host, "fe80::1%eth0");
        assert_eq!(link("ssh://[::1]/").host, "::1");
    }

    #[test]
    fn percent_decodes_user_and_host() {
        assert_eq!(
            link("ssh://jane%40corp@host").user.as_deref(),
            Some("jane@corp")
        );
        assert_eq!(link("ssh://user@my%2Dhost").host, "my-host");
    }

    #[test]
    fn ignores_a_path_query_and_fragment() {
        assert_eq!(link("ssh://host/some/path?x=1#frag"), ssh("host"));
        assert_eq!(
            link("ssh://user@host/a@b"),
            ConnectionLink {
                user: Some("user".into()),
                ..ssh("host")
            }
        );
    }

    #[test]
    fn accepts_a_sha256_fingerprint_connection_parameter() {
        assert_eq!(
            link(&format!("ssh://admin;fingerprint={SHA256}@host:2222")),
            ConnectionLink {
                port: Some(2222),
                user: Some("admin".into()),
                host_key_fingerprint: Some(SHA256.into()),
                ..ssh("host")
            }
        );
        assert_eq!(
            link(&format!(
                "ssh://;fingerprint={}@host",
                encode_uri_component(SHA256)
            )),
            ConnectionLink {
                host_key_fingerprint: Some(SHA256.into()),
                ..ssh("host")
            }
        );
    }

    #[test]
    fn keeps_an_unescaped_slash_of_a_base64_fingerprint_in_the_user_part() {
        let fingerprint = "SHA256:ab/cdefghijklmnopqrstuvwxyz0123456789ABCDEF";
        assert_eq!(
            link(&format!("ssh://admin;fingerprint={fingerprint}@host/")),
            ConnectionLink {
                user: Some("admin".into()),
                host_key_fingerprint: Some(fingerprint.into()),
                ..ssh("host")
            }
        );
    }

    #[test]
    fn accepts_the_draft_fingerprint_form_and_md5_fingerprints() {
        assert_eq!(
            link(&format!(
                "ssh://u;fingerprint=ssh-rsa-{}@host",
                MD5_PAIRS.replace(':', "-")
            ))
            .host_key_fingerprint,
            Some(format!("ssh-rsa MD5:{MD5_PAIRS}"))
        );
        assert_eq!(
            link(&format!(
                "ssh://u;fingerprint=MD5:{}@host",
                MD5_PAIRS.to_uppercase()
            ))
            .host_key_fingerprint,
            Some(format!("MD5:{MD5_PAIRS}"))
        );
    }

    #[test]
    fn ignores_connection_parameters_it_does_not_know() {
        let parsed = link(&format!("ssh://admin;color=red,fingerprint={SHA256}@host"));
        assert_eq!(parsed.user.as_deref(), Some("admin"));
        assert_eq!(parsed.host_key_fingerprint.as_deref(), Some(SHA256));
        assert_eq!(
            link("ssh://admin;x-option=1@host"),
            ConnectionLink {
                user: Some("admin".into()),
                ..ssh("host")
            }
        );
    }

    #[test]
    fn rejects_a_malformed_or_repeated_fingerprint() {
        assert!(rejection("ssh://admin;fingerprint=SHA256:short@host").contains("fingerprint"));
        assert!(rejection("ssh://admin;fingerprint@host").contains("fingerprint"));
        assert!(
            rejection(&format!(
                "ssh://a;fingerprint={SHA256},fingerprint={SHA256}@host"
            ))
            .contains("more than one fingerprint")
        );
    }

    #[test]
    fn reads_telnet_host_and_port() {
        assert_eq!(
            link("telnet://switch-01:23"),
            ConnectionLink {
                port: Some(23),
                ..telnet("switch-01")
            }
        );
        assert_eq!(link("telnet://10.0.0.5/"), telnet("10.0.0.5"));
        assert_eq!(
            link("TELNET://[::1]:2323"),
            ConnectionLink {
                port: Some(2323),
                ..telnet("::1")
            }
        );
    }

    #[test]
    fn keeps_an_advisory_telnet_user_but_no_connection_parameters() {
        assert_eq!(
            link("telnet://operator@switch"),
            ConnectionLink {
                user: Some("operator".into()),
                ..telnet("switch")
            }
        );
        assert!(
            rejection(&format!("telnet://op;fingerprint={SHA256}@switch"))
                .contains("no connection parameters")
        );
    }

    #[test]
    fn rejects_option_or_shell_injection() {
        for (url, message) in [
            ("ssh://-oProxyCommand=touch%20pwned@host", "user name"),
            ("ssh://-oProxyCommand=id", "host name"),
            ("ssh://%2DoProxyCommand=id", "host name"),
            ("ssh://admin@-host", "host name"),
            ("ssh://%2Dadmin@host", "user name"),
            ("ssh://admin@host;reboot", "host name"),
            ("ssh://adm$(id)in@host", "user name"),
            ("ssh://admin@ho%60st", "host name"),
            ("ssh://admin@.host", "host name"),
            ("telnet://-l%20root@switch", "user name"),
            ("telnet://--help", "host name"),
        ] {
            let error = rejection(url);
            assert!(error.contains(message), "{url}: {error}");
        }
    }

    #[test]
    fn rejects_passwords_in_the_link() {
        assert!(rejection("ssh://admin:secret@host").contains("password"));
        assert!(rejection("telnet://admin:@switch").contains("password"));
    }

    #[test]
    fn rejects_other_schemes() {
        for url in [
            "sftp://host",
            "https://example.com",
            "file:///etc/passwd",
            "rlogin://host",
            "host",
        ] {
            assert!(
                rejection(url).contains("only ssh:// and telnet://"),
                "{url}"
            );
        }
    }

    #[test]
    fn rejects_malformed_links() {
        for url in [
            "ssh:host",
            "ssh:/host",
            "ssh://host name",
            "ssh://ho\tst",
            "ssh://ho\nst",
            "ssh://us%ZZer@host",
            "ssh://us%C3er@host",
            "ssh://\"host\"",
        ] {
            assert!(parse_connection_link(url).is_err(), "{url:?}");
        }
    }

    #[test]
    fn rejects_links_without_a_host() {
        assert!(rejection("ssh://").contains("host"));
        assert!(rejection("ssh://admin@").contains("host"));
        assert!(rejection("ssh://admin@:22").contains("host"));
        assert!(rejection("ssh://[]").contains("host"));
    }

    #[test]
    fn rejects_broken_ipv6_literals_and_ports() {
        assert!(rejection("ssh://[2001:db8::1").contains("host name"));
        assert!(rejection("ssh://[not-ipv6]:22").contains("host name"));
        assert!(rejection("ssh://2001:db8::1").contains("port"));
        assert!(rejection("ssh://host:0").contains("port"));
        assert!(rejection("ssh://host:65536").contains("port"));
        assert!(rejection("ssh://host:22x").contains("port"));
        assert!(rejection("ssh://host:-22").contains("port"));
    }

    #[test]
    fn rejects_oversized_links_and_parts() {
        assert!(rejection(&format!("ssh://{}", "a".repeat(2050))).contains("longer than"));
        assert!(rejection(&format!("ssh://{}", "a".repeat(254))).contains("host name"));
        assert!(rejection(&format!("ssh://{}@host", "u".repeat(256))).contains("user name"));
    }

    #[test]
    fn shortens_what_it_quotes_back() {
        let error = rejection(&format!("ssh://-{}", "x".repeat(200)));
        assert!(js::utf16_len(&error) < 120, "{error}");
        assert_eq!(
            error,
            format!("“-{}…” is not a valid host name.", "x".repeat(62))
        );
    }

    #[test]
    fn normalizes_the_accepted_fingerprint_forms() {
        assert_eq!(
            normalize_host_key_fingerprint(SHA256).as_deref(),
            Some(SHA256)
        );
        assert_eq!(
            normalize_host_key_fingerprint(&format!("sha256:{}=", &SHA256[7..])).as_deref(),
            Some(SHA256)
        );
        assert_eq!(
            normalize_host_key_fingerprint("SHA256:ab-_defghijklmnopqrstuvwxyz0123456789ABCDEF")
                .as_deref(),
            Some("SHA256:ab+/defghijklmnopqrstuvwxyz0123456789ABCDEF")
        );
        assert_eq!(
            normalize_host_key_fingerprint(&format!("ssh-ed25519-{}", MD5_PAIRS.replace(':', "-"))),
            Some(format!("ssh-ed25519 MD5:{MD5_PAIRS}"))
        );
        assert_eq!(
            normalize_host_key_fingerprint(&format!(
                "ecdsa-sha2-nistp256-{}",
                MD5_PAIRS.replace(':', "-")
            )),
            Some(format!("ecdsa-sha2-nistp256 MD5:{MD5_PAIRS}"))
        );
        assert_eq!(
            normalize_host_key_fingerprint(&format!("SSH-RSA md5:{}", MD5_PAIRS.to_uppercase())),
            Some(format!("ssh-rsa MD5:{MD5_PAIRS}"))
        );
    }

    #[test]
    fn rejects_other_fingerprints() {
        for value in [
            String::new(),
            "SHA256:".into(),
            format!("{SHA256}x"),
            "SHA1:abcdef".into(),
            MD5_PAIRS.into(),
            format!("MD5:{MD5_PAIRS}:00"),
            format!("ssh-rsa-{MD5_PAIRS}"),
            format!("ssh-rsa SHA256:{}", &SHA256[7..]),
            // JavaScript's /i folds ASCII only: a Kelvin sign is not a "k".
            "\u{212A}ey MD5:c1:b1:30:29:d7:b8:de:6c:97:77:10:d7:46:41:63:87".into(),
        ] {
            assert_eq!(normalize_host_key_fingerprint(&value), None, "{value}");
        }
    }

    #[test]
    fn formats_a_quick_connect_target() {
        assert_eq!(
            connection_link_target(&link("ssh://admin@10.0.0.1:2222")),
            "admin@10.0.0.1:2222"
        );
        assert_eq!(
            connection_link_target(&link("ssh://[2001:db8::1]:830")),
            "[2001:db8::1]:830"
        );
        assert_eq!(
            connection_link_target(&link("ssh://[2001:db8::1]")),
            "2001:db8::1"
        );
        assert_eq!(connection_link_target(&link("ssh://host")), "host");
    }

    #[test]
    fn tells_links_from_files_and_switches() {
        assert!(looks_like_connection_link("ssh://host"));
        assert!(looks_like_connection_link("TELNET:switch"));
        assert!(looks_like_connection_link("https://example.com"));
        assert!(!looks_like_connection_link("/opt/Muxus/muxus"));
        assert!(!looks_like_connection_link(
            "C:\\Program Files\\Muxus\\Muxus.exe"
        ));
        assert!(!looks_like_connection_link("--host=edge"));
        assert!(!looks_like_connection_link("."));
    }

    #[test]
    fn serializes_like_the_typescript() {
        assert_eq!(
            serde_json::to_string(&link("ssh://admin@[::1]:22")).unwrap(),
            r#"{"scheme":"ssh","host":"::1","port":22,"user":"admin"}"#
        );
    }
}
