//! Just enough SOCKS5 (RFC 1928) to serve browsers and CLIs: no-auth negotiation, then a CONNECT
//! request with an IPv4, domain or IPv6 target.

use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

/// `REP` codes the forward answers with.
pub(crate) const REPLY_SUCCEEDED: u8 = 0;
pub(crate) const REPLY_CONNECTION_REFUSED: u8 = 5;
pub(crate) const REPLY_COMMAND_NOT_SUPPORTED: u8 = 7;
pub(crate) const REPLY_ADDRESS_TYPE_NOT_SUPPORTED: u8 = 8;

/// What the client asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SocksRequest {
    /// CONNECT to `host:port`; answer with [`reply`] once the channel is open (or not).
    Connect { host: String, port: u16 },
    /// The request was refused (and answered) or the client does not speak SOCKS5; hang up.
    Refused,
}

/// Reads the greeting and the request. Nothing past the request is consumed, so whatever the
/// client sent early stays in the socket for the tunnel.
pub(crate) async fn read_request<S>(socket: &mut S) -> io::Result<SocksRequest>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut greeting = [0u8; 2];
    socket.read_exact(&mut greeting).await?;
    let mut methods = vec![0u8; usize::from(greeting[1])];
    socket.read_exact(&mut methods).await?;
    if greeting[0] != 5 {
        return Ok(SocksRequest::Refused);
    }
    socket.write_all(&[5, 0]).await?; // no authentication

    let mut head = [0u8; 4];
    socket.read_exact(&mut head).await?;
    if head[0] != 5 || head[1] != 1 {
        reply(socket, REPLY_COMMAND_NOT_SUPPORTED).await?;
        let _ = socket.shutdown().await;
        return Ok(SocksRequest::Refused);
    }
    let host = match head[3] {
        1 => {
            let mut addr = [0u8; 4];
            socket.read_exact(&mut addr).await?;
            addr.iter().map(u8::to_string).collect::<Vec<_>>().join(".")
        }
        3 => {
            let mut len = [0u8; 1];
            socket.read_exact(&mut len).await?;
            let mut name = vec![0u8; usize::from(len[0])];
            socket.read_exact(&mut name).await?;
            String::from_utf8_lossy(&name).into_owned()
        }
        4 => {
            let mut addr = [0u8; 16];
            socket.read_exact(&mut addr).await?;
            // Eight unpadded hex groups, uncompressed, as the TypeScript server joined them.
            addr.chunks(2)
                .map(|pair| format!("{:x}", u16::from_be_bytes([pair[0], pair[1]])))
                .collect::<Vec<_>>()
                .join(":")
        }
        _ => {
            reply(socket, REPLY_ADDRESS_TYPE_NOT_SUPPORTED).await?;
            let _ = socket.shutdown().await;
            return Ok(SocksRequest::Refused);
        }
    };
    let mut port = [0u8; 2];
    socket.read_exact(&mut port).await?;
    Ok(SocksRequest::Connect {
        host,
        port: u16::from_be_bytes(port),
    })
}

/// +----+-----+-------+------+----------+----------+
/// |VER | REP |  RSV  | ATYP | BND.ADDR | BND.PORT |
/// +----+-----+-------+------+----------+----------+
pub(crate) async fn reply<S>(socket: &mut S, code: u8) -> io::Result<()>
where
    S: AsyncWrite + Unpin,
{
    socket.write_all(&[5, code, 0, 1, 0, 0, 0, 0, 0, 0]).await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn request(bytes: &[u8]) -> (io::Result<SocksRequest>, Vec<u8>) {
        let (mut client, mut server) = tokio::io::duplex(1024);
        client.write_all(bytes).await.unwrap();
        let result = read_request(&mut server).await;
        drop(server);
        let mut written = Vec::new();
        client.read_to_end(&mut written).await.unwrap();
        (result, written)
    }

    #[tokio::test]
    async fn reads_an_ipv4_connect() {
        let (result, written) = request(&[5, 1, 0, 5, 1, 0, 1, 10, 0, 0, 7, 0x1f, 0x90]).await;
        assert_eq!(
            result.unwrap(),
            SocksRequest::Connect {
                host: "10.0.0.7".into(),
                port: 8080
            }
        );
        assert_eq!(written, vec![5, 0]);
    }

    #[tokio::test]
    async fn reads_a_domain_connect_and_leaves_early_data_unread() {
        let (mut client, mut server) = tokio::io::duplex(1024);
        let mut bytes = vec![5, 2, 0, 2, 5, 1, 0, 3, 9];
        bytes.extend_from_slice(b"localhost");
        bytes.extend_from_slice(&[0, 80]);
        bytes.extend_from_slice(b"GET /");
        client.write_all(&bytes).await.unwrap();
        let result = read_request(&mut server).await.unwrap();
        assert_eq!(
            result,
            SocksRequest::Connect {
                host: "localhost".into(),
                port: 80
            }
        );
        let mut early = [0u8; 5];
        server.read_exact(&mut early).await.unwrap();
        assert_eq!(&early, b"GET /");
    }

    #[tokio::test]
    async fn reads_an_ipv6_connect_as_eight_hex_groups() {
        let mut bytes = vec![5, 1, 0, 5, 1, 0, 4];
        bytes.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1]);
        bytes.extend_from_slice(&[0, 22]);
        let (result, _) = request(&bytes).await;
        assert_eq!(
            result.unwrap(),
            SocksRequest::Connect {
                host: "0:0:0:0:0:0:0:1".into(),
                port: 22
            }
        );
    }

    #[tokio::test]
    async fn refuses_commands_other_than_connect() {
        // BIND (2) is not supported.
        let (result, written) = request(&[5, 1, 0, 5, 2, 0, 1, 127, 0, 0, 1, 0, 80]).await;
        assert_eq!(result.unwrap(), SocksRequest::Refused);
        assert_eq!(written, vec![5, 0, 5, 7, 0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn refuses_unknown_address_types() {
        let (result, written) = request(&[5, 1, 0, 5, 1, 0, 9]).await;
        assert_eq!(result.unwrap(), SocksRequest::Refused);
        assert_eq!(written, vec![5, 0, 5, 8, 0, 1, 0, 0, 0, 0, 0, 0]);
    }

    #[tokio::test]
    async fn hangs_up_on_clients_that_do_not_speak_socks5() {
        let (result, written) = request(&[4, 1, 0]).await;
        assert_eq!(result.unwrap(), SocksRequest::Refused);
        assert!(written.is_empty());
    }
}
