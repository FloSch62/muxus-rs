//! A tiny VNC server for the sandbox (`hack/demo-vnc.mjs`): just enough RFB 3.8 (no
//! authentication, raw encoding) to hand the client one frame of the invented desktop.

use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use crate::demo_desktop::Desktop;

#[derive(Clone, Copy)]
struct Format {
    big_endian: bool,
    red_shift: u32,
    green_shift: u32,
    blue_shift: u32,
}

/// Serve `desktop` as `name` on `listener` (call within a tokio runtime).
pub fn serve(listener: std::net::TcpListener, desktop: Arc<Desktop>, name: &'static str) {
    listener.set_nonblocking(true).unwrap();
    let listener = TcpListener::from_std(listener).unwrap();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            let desktop = desktop.clone();
            tokio::spawn(async move {
                let _ = session(socket, &desktop, name).await;
            });
        }
    });
}

async fn session(mut socket: TcpStream, desktop: &Desktop, name: &str) -> std::io::Result<()> {
    socket.set_nodelay(true)?;
    let (width, height) = (desktop.width as u16, desktop.height as u16);
    let mut format = Format {
        big_endian: false,
        red_shift: 16,
        green_shift: 8,
        blue_shift: 0,
    };
    socket.write_all(b"RFB 003.008\n").await?;
    socket.read_exact(&mut [0u8; 12]).await?;
    // One security type: None.
    socket.write_all(&[1, 1]).await?;
    socket.read_exact(&mut [0u8; 1]).await?;
    socket.write_all(&0u32.to_be_bytes()).await?;
    socket.read_exact(&mut [0u8; 1]).await?;
    let mut init = width.to_be_bytes().to_vec();
    init.extend(height.to_be_bytes());
    init.extend([32, 24, 0, 1, 0, 255, 0, 255, 0, 255, 16, 8, 0, 0, 0, 0]);
    init.extend((name.len() as u32).to_be_bytes());
    init.extend(name.as_bytes());
    socket.write_all(&init).await?;
    loop {
        let mut kind = [0u8; 1];
        socket.read_exact(&mut kind).await?;
        match kind[0] {
            0 => {
                let mut rest = [0u8; 19];
                socket.read_exact(&mut rest).await?;
                format = Format {
                    big_endian: rest[5] == 1,
                    red_shift: rest[13].into(),
                    green_shift: rest[14].into(),
                    blue_shift: rest[15].into(),
                };
            }
            2 => {
                let mut head = [0u8; 3];
                socket.read_exact(&mut head).await?;
                let count = usize::from(u16::from_be_bytes([head[1], head[2]]));
                socket.read_exact(&mut vec![0u8; count * 4]).await?;
            }
            3 => {
                let mut rest = [0u8; 9];
                socket.read_exact(&mut rest).await?;
                // Only full refreshes get a frame: the picture never changes.
                if rest[0] == 0 {
                    socket.write_all(&frame(desktop, format)).await?;
                }
            }
            4 => {
                socket.read_exact(&mut [0u8; 7]).await?;
            }
            5 => {
                socket.read_exact(&mut [0u8; 5]).await?;
            }
            6 => {
                let mut head = [0u8; 7];
                socket.read_exact(&mut head).await?;
                let length = i32::from_be_bytes([head[3], head[4], head[5], head[6]]);
                socket
                    .read_exact(&mut vec![0u8; length.unsigned_abs() as usize])
                    .await?;
            }
            _ => return Ok(()),
        }
    }
}

fn frame(desktop: &Desktop, format: Format) -> Vec<u8> {
    let (width, height) = (desktop.width as u16, desktop.height as u16);
    let mut update = vec![0, 0, 0, 1, 0, 0, 0, 0];
    update.extend(width.to_be_bytes());
    update.extend(height.to_be_bytes());
    update.extend(0u32.to_be_bytes());
    update.reserve(desktop.pixels.len() / 3 * 4);
    for rgb in desktop.pixels.as_chunks::<3>().0 {
        let value = (u32::from(rgb[0]) << format.red_shift)
            | (u32::from(rgb[1]) << format.green_shift)
            | (u32::from(rgb[2]) << format.blue_shift);
        if format.big_endian {
            update.extend(value.to_be_bytes());
        } else {
            update.extend(value.to_le_bytes());
        }
    }
    update
}
