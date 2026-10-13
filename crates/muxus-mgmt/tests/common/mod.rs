//! A gRPC target over HTTP/2 that answers whatever the test tells it to,
//! message by message (the TypeScript tests' `fakeTarget`).

#![allow(dead_code)]

use std::sync::{Arc, Mutex};

use bytes::{Buf, BufMut, Bytes, BytesMut};
use h2::server::SendResponse;
use h2::{RecvStream, SendStream};
use http::{HeaderMap, HeaderValue, Response};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::mpsc;

/// One gRPC message frame.
pub fn frame(message: &[u8]) -> Bytes {
    let mut out = BytesMut::with_capacity(message.len() + 5);
    out.put_u8(0);
    out.put_u32(message.len() as u32);
    out.put_slice(message);
    out.freeze()
}

/// Reassembles gRPC messages from DATA chunks of any size.
#[derive(Default)]
struct FrameDecoder {
    buffer: BytesMut,
}

impl FrameDecoder {
    fn push(&mut self, chunk: &[u8]) -> Vec<Bytes> {
        self.buffer.extend_from_slice(chunk);
        let mut messages = Vec::new();
        while self.buffer.len() >= 5 {
            let length = u32::from_be_bytes(self.buffer[1..5].try_into().unwrap()) as usize;
            if self.buffer.len() < 5 + length {
                break;
            }
            self.buffer.advance(5);
            messages.push(self.buffer.split_to(length).freeze());
        }
        messages
    }
}

/// A call as the fake target sees it.
pub struct Call {
    pub path: String,
    pub headers: HeaderMap,
    requests: mpsc::UnboundedReceiver<Bytes>,
    /// Every request message received so far, for checks after the call.
    pub received: Arc<Mutex<Vec<Bytes>>>,
    respond: Option<SendResponse<Bytes>>,
    send: Option<SendStream<Bytes>>,
}

impl Call {
    /// The next request message; `None` once the client ended its side.
    pub async fn next_request(&mut self) -> Option<Bytes> {
        self.requests.recv().await
    }

    /// Wait for the client to end its side.
    pub async fn ended(&mut self) {
        while self.requests.recv().await.is_some() {}
    }

    /// Send the response headers.
    pub fn start(&mut self) {
        if let Some(mut respond) = self.respond.take() {
            let response = Response::builder()
                .status(200)
                .header("content-type", "application/grpc")
                .body(())
                .unwrap();
            self.send = Some(respond.send_response(response, false).unwrap());
        }
    }

    pub fn send(&mut self, message: &[u8]) {
        self.start();
        if let Some(send) = self.send.as_mut() {
            send.reserve_capacity(message.len() + 5);
            let _ = send.send_data(frame(message), false);
        }
    }

    /// Trailers with the status; ends the call.
    pub fn finish(&mut self, status: i32, message: Option<&str>) {
        self.start();
        if let Some(mut send) = self.send.take() {
            let mut trailers = HeaderMap::new();
            trailers.insert("grpc-status", HeaderValue::from(status));
            if let Some(message) = message {
                trailers.insert("grpc-message", HeaderValue::from_str(message).unwrap());
            }
            let _ = send.send_trailers(trailers);
        }
    }

    /// Messages, then the status.
    pub fn reply(&mut self, messages: &[&[u8]], status: i32, message: Option<&str>) {
        for payload in messages {
            self.send(payload);
        }
        self.finish(status, message);
    }

    /// A non-gRPC HTTP answer.
    pub fn http_status(&mut self, status: u16) {
        if let Some(mut respond) = self.respond.take() {
            let response = Response::builder().status(status).body(()).unwrap();
            let _ = respond.send_response(response, true);
        }
    }
}

async fn read_requests(
    mut body: RecvStream,
    sender: mpsc::UnboundedSender<Bytes>,
    received: Arc<Mutex<Vec<Bytes>>>,
) {
    let mut decoder = FrameDecoder::default();
    while let Some(chunk) = body.data().await {
        let Ok(chunk) = chunk else { break };
        let _ = body.flow_control().release_capacity(chunk.len());
        for message in decoder.push(&chunk) {
            received.lock().unwrap().push(message.clone());
            let _ = sender.send(message);
        }
    }
}

/// Serve HTTP/2 on one connection, handing each call to `handle`.
pub async fn serve<IO, F, Fut>(io: IO, handle: F)
where
    IO: AsyncRead + AsyncWrite + Unpin,
    F: Fn(Call) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let Ok(mut connection) = h2::server::Builder::new()
        .initial_window_size(16 * 1024 * 1024)
        .handshake::<_, Bytes>(io)
        .await
    else {
        return;
    };
    let handle = Arc::new(handle);
    while let Some(Ok((request, respond))) = connection.accept().await {
        let (parts, body) = request.into_parts();
        let (sender, requests) = mpsc::unbounded_channel();
        let received = Arc::new(Mutex::new(Vec::new()));
        tokio::spawn(read_requests(body, sender, received.clone()));
        let call = Call {
            path: parts.uri.path().to_owned(),
            headers: parts.headers,
            requests,
            received,
            respond: Some(respond),
            send: None,
        };
        tokio::spawn(handle(call));
    }
}

/// A plain-text target on loopback; returns a connected socket and the port.
pub async fn plaintext_target<F, Fut>(handle: F) -> (TcpStream, u16)
where
    F: Fn(Call) -> Fut + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        serve(socket, handle).await;
    });
    let socket = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    (socket, port)
}

/// A plain-text target that takes any number of connections; returns its port.
pub async fn listen<F, Fut>(handle: F) -> u16
where
    F: Fn(Call) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(serve(socket, handle.clone()));
        }
    });
    port
}
