//! Reader and writer for the binary Xauthority format (libXau). Each record is five big-endian
//! length-prefixed fields: a u16 address family, then the address, display number, auth protocol
//! name and auth data as u16-length byte strings.

use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::path::{Path, PathBuf};

use crate::local_env::LocalEnv;

pub const FAMILY_INTERNET: u16 = 0;
pub const FAMILY_INTERNET6: u16 = 6;
/// Unix-domain and loopback connections; the address is the local hostname.
pub const FAMILY_LOCAL: u16 = 256;
/// Matches any address.
pub const FAMILY_WILD: u16 = 65535;

pub const MIT_MAGIC_COOKIE: &str = "MIT-MAGIC-COOKIE-1";

/// One Xauthority record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XauthEntry {
    pub family: u16,
    pub address: Vec<u8>,
    /// Display number as written by xauth ("0"); empty matches any display.
    pub number: String,
    pub name: String,
    pub data: Vec<u8>,
}

/// Credentials presented to an X server in the connection setup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct X11Auth {
    pub name: String,
    pub data: Vec<u8>,
}

impl X11Auth {
    /// MIT-MAGIC-COOKIE-1 credentials with `cookie` as their data.
    pub fn magic_cookie(cookie: impl Into<Vec<u8>>) -> Self {
        Self {
            name: MIT_MAGIC_COOKIE.to_string(),
            data: cookie.into(),
        }
    }
}

/// Parse an Xauthority file. A truncated trailing record is ignored, as libXau does.
pub fn parse_xauthority(buf: &[u8]) -> Vec<XauthEntry> {
    fn u16_at(buf: &[u8], offset: usize) -> u16 {
        u16::from_be_bytes([buf[offset], buf[offset + 1]])
    }
    fn field<'a>(buf: &'a [u8], offset: &mut usize) -> Option<&'a [u8]> {
        if *offset + 2 > buf.len() {
            return None;
        }
        let length = usize::from(u16_at(buf, *offset));
        *offset += 2;
        if *offset + length > buf.len() {
            return None;
        }
        let value = &buf[*offset..*offset + length];
        *offset += length;
        Some(value)
    }
    let mut entries = Vec::new();
    let mut offset = 0usize;
    while offset + 2 <= buf.len() {
        let family = u16_at(buf, offset);
        offset += 2;
        let address = field(buf, &mut offset);
        let number = field(buf, &mut offset);
        let name = field(buf, &mut offset);
        let data = field(buf, &mut offset);
        // A truncated trailing record is ignored, as libXau does.
        let (Some(address), Some(number), Some(name), Some(data)) = (address, number, name, data)
        else {
            break;
        };
        entries.push(XauthEntry {
            family,
            address: address.to_vec(),
            number: latin1_decode(number),
            name: latin1_decode(name),
            data: data.to_vec(),
        });
    }
    entries
}

/// Write records in the libXau format. A field longer than 65535 bytes cannot be represented and
/// fails with `InvalidInput`.
pub fn serialize_xauthority(entries: &[XauthEntry]) -> io::Result<Vec<u8>> {
    fn field(out: &mut Vec<u8>, value: &[u8]) -> io::Result<()> {
        let length = u16::try_from(value.len()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidInput, "Xauthority field too long")
        })?;
        out.extend_from_slice(&length.to_be_bytes());
        out.extend_from_slice(value);
        Ok(())
    }
    let mut out = Vec::new();
    for entry in entries {
        out.extend_from_slice(&entry.family.to_be_bytes());
        field(&mut out, &entry.address)?;
        field(&mut out, &latin1_encode(&entry.number))?;
        field(&mut out, &latin1_encode(&entry.name))?;
        field(&mut out, &entry.data)?;
    }
    Ok(out)
}

/// $XAUTHORITY, else ~/.Xauthority — the lookup order Xlib uses. `env` reads one environment
/// variable; the home directory comes from its `HOME`, else from the account database.
pub fn xauthority_path(env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    if let Some(path) = env("XAUTHORITY").filter(|value| !value.is_empty()) {
        return PathBuf::from(path);
    }
    let home = env("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| LocalEnv::current().home);
    home.join(".Xauthority")
}

/// Where an X client is connected, for picking the matching Xauthority record: a local display
/// (Unix socket or loopback TCP) or the IP address of a remote TCP display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum XauthTarget {
    Local,
    Remote { family: u16, address: Vec<u8> },
}

/// The Xauthority address for a connected TCP display, from the socket's peer address as libxcb
/// derives it: loopback counts as local, IPv4 (including IPv4-mapped IPv6) is FamilyInternet,
/// other IPv6 is FamilyInternet6. Using the peer address covers DISPLAY host names without a
/// separate lookup.
///
/// Takes the textual form (a `%zone` suffix is ignored); an address that does not parse matches
/// only records with an empty FamilyInternet address.
pub fn xauth_target_for_peer(address: Option<&str>) -> XauthTarget {
    let ip = address.unwrap_or("");
    let ip = ip.split_once('%').map_or(ip, |(head, _)| head);
    if let Ok(v4) = ip.parse::<Ipv4Addr>() {
        return xauth_target_for_ip(IpAddr::V4(v4));
    }
    if let Ok(v6) = ip.parse::<Ipv6Addr>() {
        return xauth_target_for_ip(IpAddr::V6(v6));
    }
    XauthTarget::Remote {
        family: FAMILY_INTERNET,
        address: Vec::new(),
    }
}

/// [`xauth_target_for_peer`] for an already parsed peer address.
pub fn xauth_target_for_ip(ip: IpAddr) -> XauthTarget {
    let v4_target = |v4: Ipv4Addr| {
        let bytes = v4.octets();
        if bytes[0] == 127 {
            XauthTarget::Local
        } else {
            XauthTarget::Remote {
                family: FAMILY_INTERNET,
                address: bytes.to_vec(),
            }
        }
    };
    match ip {
        IpAddr::V4(v4) => v4_target(v4),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4_target(v4),
            None if v6 == Ipv6Addr::LOCALHOST => XauthTarget::Local,
            None => XauthTarget::Remote {
                family: FAMILY_INTERNET6,
                address: v6.octets().to_vec(),
            },
        },
    }
}

/// The MIT-MAGIC-COOKIE-1 credentials Xlib would send for `number`, following
/// XauGetBestAuthByAddr: a record matches on display number (empty = any) and on address —
/// FamilyWild, FamilyLocal with this machine's hostname for local displays, or the peer's IP
/// address for remote TCP displays.
pub fn find_xauth_cookie(
    entries: &[XauthEntry],
    number: &str,
    target: &XauthTarget,
    hostname: &str,
) -> Option<X11Auth> {
    let local = latin1_encode(hostname);
    let local_short = latin1_encode(hostname.split('.').next().unwrap_or(hostname));
    let matches_address = |entry: &XauthEntry| -> bool {
        if entry.family == FAMILY_WILD {
            return true;
        }
        match target {
            XauthTarget::Local => {
                entry.family == FAMILY_LOCAL
                    && (entry.address == local || entry.address == local_short)
            }
            XauthTarget::Remote { family, address } => {
                entry.family == *family && entry.address == *address
            }
        }
    };
    entries
        .iter()
        .find(|candidate| {
            candidate.name == MIT_MAGIC_COOKIE
                && (candidate.number.is_empty() || candidate.number == number)
                && matches_address(candidate)
        })
        .map(|entry| X11Auth {
            name: entry.name.clone(),
            data: entry.data.clone(),
        })
}

/// Read and search the Xauthority file; a missing or unreadable file yields no cookie.
pub async fn read_xauth_cookie(
    number: &str,
    target: &XauthTarget,
    file: &Path,
    hostname: &str,
) -> Option<X11Auth> {
    let buf = tokio::fs::read(file).await.ok()?;
    find_xauth_cookie(&parse_xauthority(&buf), number, target, hostname)
}

/// Bytes as Latin-1 text, the way Node's `latin1` encoding maps them.
fn latin1_decode(bytes: &[u8]) -> String {
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// Latin-1 bytes of a string; like Node, characters above U+00FF keep their low byte.
fn latin1_encode(text: &str) -> Vec<u8> {
    text.chars()
        .map(|ch| (u32::from(ch) & 0xff) as u8)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x11::test_support::ipv6;

    fn cookie(byte: u8) -> Vec<u8> {
        vec![byte; 16]
    }

    fn entry() -> XauthEntry {
        XauthEntry {
            family: FAMILY_LOCAL,
            address: b"workstation".to_vec(),
            number: "0".into(),
            name: MIT_MAGIC_COOKIE.into(),
            data: cookie(1),
        }
    }

    #[test]
    fn round_trips_the_libxau_record_format() {
        let entries = vec![
            entry(),
            XauthEntry {
                family: FAMILY_WILD,
                address: Vec::new(),
                number: "10".into(),
                data: cookie(2),
                ..entry()
            },
        ];
        let bytes = serialize_xauthority(&entries).unwrap();
        assert_eq!(parse_xauthority(&bytes), entries);
    }

    #[test]
    fn ignores_a_truncated_trailing_record() {
        let buf = serialize_xauthority(&[entry()]).unwrap();
        let mut truncated = buf.clone();
        truncated.extend_from_slice(&buf[..9]);
        assert_eq!(parse_xauthority(&truncated).len(), 1);
    }

    #[test]
    fn rejects_fields_the_format_cannot_hold() {
        let huge = XauthEntry {
            data: vec![0; 70_000],
            ..entry()
        };
        assert_eq!(
            serialize_xauthority(&[huge]).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn picks_the_record_xlib_would_use_for_a_display() {
        let entries = vec![
            XauthEntry {
                address: b"other-host".to_vec(),
                data: cookie(9),
                ..entry()
            },
            XauthEntry {
                number: "1".into(),
                data: cookie(8),
                ..entry()
            },
            XauthEntry {
                data: cookie(1),
                ..entry()
            },
            XauthEntry {
                family: FAMILY_INTERNET,
                address: vec![10, 0, 0, 5],
                data: cookie(3),
                ..entry()
            },
            XauthEntry {
                family: FAMILY_INTERNET6,
                address: ipv6("fd00::5"),
                data: cookie(6),
                ..entry()
            },
        ];
        let data = |number: &str, target: &XauthTarget, hostname: &str| {
            find_xauth_cookie(&entries, number, target, hostname).map(|auth| auth.data)
        };
        assert_eq!(
            data("0", &XauthTarget::Local, "workstation.example.com"),
            Some(cookie(1))
        );
        assert_eq!(
            data("1", &XauthTarget::Local, "workstation"),
            Some(cookie(8))
        );
        assert_eq!(
            data("0", &xauth_target_for_peer(Some("10.0.0.5")), "workstation"),
            Some(cookie(3))
        );
        assert_eq!(
            data("0", &xauth_target_for_peer(Some("fd00::5")), "workstation"),
            Some(cookie(6))
        );
        assert_eq!(data("2", &XauthTarget::Local, "workstation"), None);

        let wild = [XauthEntry {
            family: FAMILY_WILD,
            number: String::new(),
            data: cookie(4),
            ..entry()
        }];
        assert_eq!(
            find_xauth_cookie(&wild, "7", &XauthTarget::Local, "x").map(|auth| auth.data),
            Some(cookie(4))
        );
    }

    #[test]
    fn derives_the_record_address_from_the_connected_peer_as_libxcb_does() {
        assert_eq!(xauth_target_for_peer(Some("127.0.0.1")), XauthTarget::Local);
        assert_eq!(xauth_target_for_peer(Some("::1")), XauthTarget::Local);
        assert_eq!(
            xauth_target_for_peer(Some("::ffff:127.0.0.1")),
            XauthTarget::Local
        );
        let lan = XauthTarget::Remote {
            family: FAMILY_INTERNET,
            address: vec![192, 168, 1, 20],
        };
        assert_eq!(xauth_target_for_peer(Some("192.168.1.20")), lan);
        assert_eq!(xauth_target_for_peer(Some("::ffff:192.168.1.20")), lan);
        assert_eq!(
            xauth_target_for_peer(Some("fe80::1:2%eth0")),
            XauthTarget::Remote {
                family: FAMILY_INTERNET6,
                address: ipv6("fe80::1:2"),
            }
        );
        let XauthTarget::Remote { family, address } =
            xauth_target_for_peer(Some("2001:db8::10.0.0.1"))
        else {
            panic!("expected a remote target");
        };
        assert_eq!(family, FAMILY_INTERNET6);
        assert_eq!(address[12..], [10, 0, 0, 1]);
    }

    #[test]
    fn unknown_peers_match_only_an_empty_internet_address() {
        let unknown = XauthTarget::Remote {
            family: FAMILY_INTERNET,
            address: Vec::new(),
        };
        assert_eq!(xauth_target_for_peer(None), unknown);
        assert_eq!(xauth_target_for_peer(Some("not an address")), unknown);
    }

    #[test]
    fn finds_the_xauthority_file_like_xlib() {
        let env = |vars: &'static [(&'static str, &'static str)]| {
            move |name: &str| {
                vars.iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| value.to_string())
            }
        };
        assert_eq!(
            xauthority_path(&env(&[("XAUTHORITY", "/run/x/auth"), ("HOME", "/home/u")])),
            PathBuf::from("/run/x/auth")
        );
        assert_eq!(
            xauthority_path(&env(&[("XAUTHORITY", ""), ("HOME", "/home/u")])),
            PathBuf::from("/home/u/.Xauthority")
        );
    }

    #[tokio::test]
    async fn a_missing_xauthority_file_yields_no_cookie() {
        let dir = tempfile::tempdir().unwrap();
        let missing = dir.path().join("Xauthority");
        assert_eq!(
            read_xauth_cookie("0", &XauthTarget::Local, &missing, "x").await,
            None
        );
    }

    #[test]
    fn keeps_latin1_names_byte_exact() {
        let odd = XauthEntry {
            number: "\u{e9}".into(),
            ..entry()
        };
        let bytes = serialize_xauthority(std::slice::from_ref(&odd)).unwrap();
        assert!(bytes.windows(3).any(|w| w == [0, 1, 0xe9]));
        assert_eq!(parse_xauthority(&bytes), vec![odd]);
    }
}
