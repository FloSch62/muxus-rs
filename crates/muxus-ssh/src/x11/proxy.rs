//! The X11 connection-setup parser and the cookie-swapping relay between a forwarded channel and
//! the local X server.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::X11Error;
use super::xauthority::{MIT_MAGIC_COOKIE, X11Auth};

/// Setup requests carry two short strings; anything larger is not X11.
pub const MAX_SETUP_BYTES: usize = 64 * 1024;
const SETUP_HEADER_BYTES: usize = 12;

/// Rejection reasons, as the proxy reports them.
pub const REJECT_OVERSIZED: &str = "oversized connection setup";
pub const REJECT_NOT_X11: &str = "not an X11 connection setup";
pub const REJECT_WRONG_COOKIE: &str = "wrong X11 authentication cookie";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum X11SetupParse {
    Incomplete,
    Invalid,
    Complete {
        little_endian: bool,
        auth_name: String,
        auth_data: Vec<u8>,
        /// Bytes the setup request occupies, padding included.
        length: usize,
    },
}

const fn pad4(n: usize) -> usize {
    (n + 3) & !3
}

/// Parse the X11 connection setup request a client sends first: byte order ('B' or 'l'),
/// protocol version, then the authorization protocol name and data, each padded to four bytes.
pub fn parse_x11_setup(buf: &[u8]) -> X11SetupParse {
    let Some(&order) = buf.first() else {
        return X11SetupParse::Incomplete;
    };
    if order != b'B' && order != b'l' {
        return X11SetupParse::Invalid;
    }
    if buf.len() < SETUP_HEADER_BYTES {
        return X11SetupParse::Incomplete;
    }
    let little_endian = order == b'l';
    let u16_at = |offset: usize| {
        let bytes = [buf[offset], buf[offset + 1]];
        usize::from(if little_endian {
            u16::from_le_bytes(bytes)
        } else {
            u16::from_be_bytes(bytes)
        })
    };
    let name_length = u16_at(6);
    let data_length = u16_at(8);
    let length = SETUP_HEADER_BYTES + pad4(name_length) + pad4(data_length);
    if buf.len() < length {
        return X11SetupParse::Incomplete;
    }
    let name_start = SETUP_HEADER_BYTES;
    let data_start = name_start + pad4(name_length);
    X11SetupParse::Complete {
        little_endian,
        auth_name: buf[name_start..name_start + name_length]
            .iter()
            .map(|&byte| char::from(byte))
            .collect(),
        auth_data: buf[data_start..data_start + data_length].to_vec(),
        length,
    }
}

/// Rebuild a setup request with different credentials (none when `auth` is absent). Only the
/// first six bytes of `setup` (byte order and protocol version) are kept.
pub fn rewrite_x11_setup(setup: &[u8], little_endian: bool, auth: Option<&X11Auth>) -> Vec<u8> {
    // The setup's length fields are u16; credentials never come near that (Xauthority fields
    // are u16-sized too), so anything longer is cut rather than corrupting the request.
    let mut name: Vec<u8> = auth
        .map(|auth| {
            auth.name
                .chars()
                .map(|ch| (u32::from(ch) & 0xff) as u8)
                .collect()
        })
        .unwrap_or_default();
    name.truncate(usize::from(u16::MAX));
    let data = auth.map_or(&[][..], |auth| {
        &auth.data[..auth.data.len().min(usize::from(u16::MAX))]
    });
    let mut out = vec![0u8; SETUP_HEADER_BYTES + pad4(name.len()) + pad4(data.len())];
    let keep = setup.len().min(6);
    out[..keep].copy_from_slice(&setup[..keep]);
    let (name_length, data_length) = (name.len() as u16, data.len() as u16);
    let (name_bytes, data_bytes) = if little_endian {
        (name_length.to_le_bytes(), data_length.to_le_bytes())
    } else {
        (name_length.to_be_bytes(), data_length.to_be_bytes())
    };
    out[6..8].copy_from_slice(&name_bytes);
    out[8..10].copy_from_slice(&data_bytes);
    out[SETUP_HEADER_BYTES..SETUP_HEADER_BYTES + name.len()].copy_from_slice(&name);
    let data_start = SETUP_HEADER_BYTES + pad4(name.len());
    out[data_start..data_start + data.len()].copy_from_slice(data);
    out
}

/// Constant-time comparison of equally long byte strings.
fn timing_safe_equal(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Join a forwarded X11 channel to the local X server the way ssh(1) does: the remote side must
/// present the fake cookie Muxus sent in x11-req, which is swapped for the local server's real
/// credentials (`auth`, none when the server needs none) before any bytes reach it. Anything else
/// closes both streams and returns [`X11Error::Rejected`].
///
/// Returns once the connection is over: when the X server closes, or either side fails. A remote
/// EOF half-closes the X server connection and keeps relaying its replies until it closes too. A
/// remote that closes before completing the setup ends the connection quietly (`Ok`). Both
/// streams are dropped on return.
pub async fn splice_x11_connection<R, L>(
    remote: R,
    local: L,
    cookie: &[u8],
    auth: Option<&X11Auth>,
) -> Result<(), X11Error>
where
    R: AsyncRead + AsyncWrite + Unpin,
    L: AsyncRead + AsyncWrite + Unpin,
{
    let mut remote = remote;
    let mut local = local;
    let mut pending: Vec<u8> = Vec::new();
    let mut chunk = vec![0u8; 16 * 1024];
    let (little_endian, setup_length) = loop {
        let read = remote.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        pending.extend_from_slice(&chunk[..read]);
        match parse_x11_setup(&pending) {
            X11SetupParse::Incomplete => {
                if pending.len() > MAX_SETUP_BYTES {
                    return Err(X11Error::Rejected(REJECT_OVERSIZED));
                }
            }
            X11SetupParse::Invalid => return Err(X11Error::Rejected(REJECT_NOT_X11)),
            X11SetupParse::Complete {
                little_endian,
                auth_name,
                auth_data,
                length,
            } => {
                if auth_name != MIT_MAGIC_COOKIE || !timing_safe_equal(&auth_data, cookie) {
                    return Err(X11Error::Rejected(REJECT_WRONG_COOKIE));
                }
                break (little_endian, length);
            }
        }
    };
    let mut first = rewrite_x11_setup(&pending, little_endian, auth);
    first.extend_from_slice(&pending[setup_length..]);
    local.write_all(&first).await?;
    local.flush().await?;
    relay(remote, local).await?;
    Ok(())
}

/// Copy both ways until the X server's side ends; a remote EOF only half-closes the X server.
async fn relay<R, L>(remote: R, local: L) -> io::Result<()>
where
    R: AsyncRead + AsyncWrite + Unpin,
    L: AsyncRead + AsyncWrite + Unpin,
{
    let (mut remote_read, mut remote_write) = tokio::io::split(remote);
    let (mut local_read, mut local_write) = tokio::io::split(local);
    let upstream = async {
        tokio::io::copy(&mut remote_read, &mut local_write).await?;
        local_write.shutdown().await
    };
    let downstream = async {
        tokio::io::copy(&mut local_read, &mut remote_write).await?;
        remote_write.shutdown().await
    };
    tokio::pin!(upstream, downstream);
    let mut upstream_done = false;
    loop {
        tokio::select! {
            result = &mut upstream, if !upstream_done => {
                result?;
                upstream_done = true;
            }
            result = &mut downstream => return result,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x11::test_support::setup_request;
    use tokio::io::duplex;

    #[test]
    fn parses_and_rebuilds_setup_requests_in_both_byte_orders() {
        for little_endian in [true, false] {
            let request = setup_request(little_endian, MIT_MAGIC_COOKIE, &[7; 16]);
            assert_eq!(request.len(), 12 + 20 + 16);
            assert_eq!(
                parse_x11_setup(&request),
                X11SetupParse::Complete {
                    little_endian,
                    auth_name: MIT_MAGIC_COOKIE.into(),
                    auth_data: vec![7; 16],
                    length: request.len(),
                }
            );
            let stripped = rewrite_x11_setup(&request, little_endian, None);
            assert_eq!(stripped.len(), 12);
            assert_eq!(stripped[..6], request[..6]);
            assert!(matches!(
                parse_x11_setup(&stripped),
                X11SetupParse::Complete { auth_name, auth_data, .. }
                    if auth_name.is_empty() && auth_data.is_empty()
            ));
        }
    }

    #[test]
    fn waits_for_a_complete_request_and_rejects_non_x11_data() {
        let request = setup_request(true, MIT_MAGIC_COOKIE, &[0; 16]);
        assert_eq!(parse_x11_setup(&request[..20]), X11SetupParse::Incomplete);
        assert_eq!(parse_x11_setup(&request[..5]), X11SetupParse::Incomplete);
        assert_eq!(parse_x11_setup(&[]), X11SetupParse::Incomplete);
        assert_eq!(
            parse_x11_setup(b"GET / HTTP/1.1\r\n"),
            X11SetupParse::Invalid
        );
    }

    const FAKE: [u8; 16] = [0xaa; 16];

    fn real() -> X11Auth {
        X11Auth::magic_cookie(vec![0x55; 16])
    }

    #[tokio::test]
    async fn swaps_the_fake_cookie_for_the_real_one_and_then_relays_both_ways() {
        let (remote, mut remote_client) = duplex(1024);
        let (local, mut x_server) = duplex(1024);
        let auth = real();
        let splice =
            tokio::spawn(
                async move { splice_x11_connection(remote, local, &FAKE, Some(&auth)).await },
            );

        // The setup and the first request may arrive split across channel packets.
        let mut setup = setup_request(true, MIT_MAGIC_COOKIE, &FAKE);
        let first_request = [1u8, 2, 3, 4];
        setup.extend_from_slice(&first_request);
        remote_client.write_all(&setup[..10]).await.unwrap();
        remote_client.flush().await.unwrap();
        tokio::task::yield_now().await;
        remote_client.write_all(&setup[10..]).await.unwrap();

        let expected_length = 12 + 20 + 16;
        let mut forwarded = vec![0u8; expected_length + first_request.len()];
        x_server.read_exact(&mut forwarded).await.unwrap();
        assert!(matches!(
            parse_x11_setup(&forwarded),
            X11SetupParse::Complete { auth_name, auth_data, .. }
                if auth_name == MIT_MAGIC_COOKIE && auth_data == real().data
        ));
        assert_eq!(forwarded[expected_length..], first_request);

        x_server.write_all(b"reply").await.unwrap();
        let mut reply = [0u8; 5];
        remote_client.read_exact(&mut reply).await.unwrap();
        assert_eq!(&reply, b"reply");

        // The X server closing ends the connection.
        drop(x_server);
        splice.await.unwrap().unwrap();
        let mut rest = Vec::new();
        remote_client.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
    }

    #[tokio::test]
    async fn closes_both_sides_when_the_remote_presents_the_wrong_cookie() {
        let (remote, mut remote_client) = duplex(1024);
        let (local, mut x_server) = duplex(1024);
        remote_client
            .write_all(&setup_request(false, MIT_MAGIC_COOKIE, &[0xab; 16]))
            .await
            .unwrap();
        let result = splice_x11_connection(remote, local, &FAKE, Some(&real())).await;
        assert!(matches!(
            result,
            Err(X11Error::Rejected(REJECT_WRONG_COOKIE))
        ));
        assert_eq!(
            result.unwrap_err().to_string(),
            "wrong X11 authentication cookie"
        );
        // Both streams were dropped: each peer sees EOF, and the X server got no bytes at all.
        let mut seen = Vec::new();
        x_server.read_to_end(&mut seen).await.unwrap();
        assert!(seen.is_empty());
        let mut back = Vec::new();
        remote_client.read_to_end(&mut back).await.unwrap();
        assert!(back.is_empty());
    }

    #[tokio::test]
    async fn rejects_other_protocols_and_cookies_of_another_kind() {
        let (remote, mut remote_client) = duplex(1024);
        let (local, _x_server) = duplex(1024);
        remote_client.write_all(b"SSH-2.0-x\r\n").await.unwrap();
        let result = splice_x11_connection(remote, local, &FAKE, None).await;
        assert!(matches!(result, Err(X11Error::Rejected(REJECT_NOT_X11))));

        let (remote, mut remote_client) = duplex(1024);
        let (local, _x_server) = duplex(1024);
        remote_client
            .write_all(&setup_request(true, "XDM-AUTHORIZATION-1", &FAKE))
            .await
            .unwrap();
        let result = splice_x11_connection(remote, local, &FAKE, None).await;
        assert!(matches!(
            result,
            Err(X11Error::Rejected(REJECT_WRONG_COOKIE))
        ));
    }

    #[tokio::test]
    async fn rejects_an_oversized_setup() {
        let (remote, mut remote_client) = duplex(256 * 1024);
        let (local, _x_server) = duplex(1024);
        // Claims 65535-byte name and data: the request never completes within the limit.
        let mut header = vec![b'l', 0, 11, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 0, 0];
        header.resize(MAX_SETUP_BYTES + 100, 0);
        remote_client.write_all(&header).await.unwrap();
        let result = splice_x11_connection(remote, local, &FAKE, None).await;
        assert!(matches!(result, Err(X11Error::Rejected(REJECT_OVERSIZED))));
    }

    #[tokio::test]
    async fn a_remote_that_closes_before_the_setup_ends_quietly() {
        let (remote, mut remote_client) = duplex(1024);
        let (local, mut x_server) = duplex(1024);
        remote_client.write_all(&[b'l', 0, 11]).await.unwrap();
        drop(remote_client);
        splice_x11_connection(remote, local, &FAKE, None)
            .await
            .unwrap();
        let mut seen = Vec::new();
        x_server.read_to_end(&mut seen).await.unwrap();
        assert!(seen.is_empty());
    }

    #[tokio::test]
    async fn strips_credentials_when_the_x_server_needs_none() {
        let (remote, mut remote_client) = duplex(1024);
        let (local, mut x_server) = duplex(1024);
        let splice =
            tokio::spawn(async move { splice_x11_connection(remote, local, &FAKE, None).await });
        remote_client
            .write_all(&setup_request(false, MIT_MAGIC_COOKIE, &FAKE))
            .await
            .unwrap();
        let mut forwarded = [0u8; 12];
        x_server.read_exact(&mut forwarded).await.unwrap();
        assert!(matches!(
            parse_x11_setup(&forwarded),
            X11SetupParse::Complete { little_endian: false, auth_name, auth_data, length: 12 }
                if auth_name.is_empty() && auth_data.is_empty()
        ));
        // A remote EOF half-closes the X server, which still gets to answer.
        remote_client.shutdown().await.unwrap();
        let mut rest = Vec::new();
        x_server.read_to_end(&mut rest).await.unwrap();
        assert!(rest.is_empty());
        x_server.write_all(b"bye").await.unwrap();
        let mut answer = [0u8; 3];
        remote_client.read_exact(&mut answer).await.unwrap();
        assert_eq!(&answer, b"bye");
        drop(x_server);
        splice.await.unwrap().unwrap();
    }
}
