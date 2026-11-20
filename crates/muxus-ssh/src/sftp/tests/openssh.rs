//! The resumable transfers and file modes against OpenSSH's own SFTP server, in the container
//! of `hack/e2e/containers/sshd`. The connection goes through a small proxy in this process
//! that cuts it after a number of bytes, as a link that drops does.
//!
//! ```bash
//! docker build -t muxusrs-e2e-sshd hack/e2e/containers/sshd
//! docker run -d --rm --name muxusrs-sftp-check -p 127.0.0.1:22211:22 muxusrs-e2e-sshd
//! MUXUS_E2E_SSHD=127.0.0.1:22211 cargo test -p muxus-ssh --lib sftp::tests::openssh -- --ignored
//! ```
//!
//! `MUXUS_E2E_SSHD_UMASK` (octal, 022 by default) is the umask the server's SFTP runs with, for
//! a container started with another `Subsystem sftp internal-sftp -u <umask>`.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};

use super::resume::staged_name;
use super::*;
use crate::Problem;
use crate::connection::BoxFuture;
use crate::sftp::progress::{CancelSignal, UploadStep};
use crate::sftp::service::{DownloadRange, UploadOptions};
use crate::sftp::upload::{
    ConfirmRequest, DropPayload, DroppedFile, SftpUploadDeps, UploadResult, upload_source,
    upload_to_remote,
};

const MIB: usize = 1024 * 1024;

fn server() -> (String, u16) {
    let address = std::env::var("MUXUS_E2E_SSHD").unwrap_or_else(|_| "127.0.0.1:22201".into());
    let (host, port) = address
        .rsplit_once(':')
        .expect("MUXUS_E2E_SSHD is host:port");
    (host.to_string(), port.parse().expect("a port"))
}

fn umask() -> u32 {
    let text = std::env::var("MUXUS_E2E_SSHD_UMASK").unwrap_or_else(|_| "022".into());
    u32::from_str_radix(&text, 8).expect("MUXUS_E2E_SSHD_UMASK is octal")
}

/// Bytes with no pattern the server's tail check could mistake.
fn random_bytes(len: usize, seed: u64) -> Vec<u8> {
    let mut state = seed | 1;
    (0..len)
        .map(|_| {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 32) as u8
        })
        .collect()
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A TCP proxy to the server that counts what passes and can cut a connection. Every login
/// through it reaches the server at the same address, as reconnecting to a host does.
struct Proxy {
    port: u16,
    /// Bytes from the clients to the server, and back.
    sent: AtomicU64,
    received: AtomicU64,
    /// A connection is closed once it carried this many bytes to the server, or back; 0 never.
    cut_sent: AtomicU64,
    cut_received: AtomicU64,
}

impl Proxy {
    async fn start() -> Arc<Self> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let proxy = Arc::new(Self {
            port: listener.local_addr().unwrap().port(),
            sent: AtomicU64::new(0),
            received: AtomicU64::new(0),
            cut_sent: AtomicU64::new(0),
            cut_received: AtomicU64::new(0),
        });
        let shared = Arc::downgrade(&proxy);
        tokio::spawn(async move {
            let (host, upstream_port) = server();
            while let Ok((client, _)) = listener.accept().await {
                let Some(proxy) = shared.upgrade() else { break };
                let upstream = TcpStream::connect((host.as_str(), upstream_port))
                    .await
                    .expect("the container answers");
                tokio::spawn(proxy.carry(client, upstream));
            }
        });
        proxy
    }

    async fn carry(self: Arc<Self>, client: TcpStream, upstream: TcpStream) {
        let (client_read, client_write) = client.into_split();
        let (server_read, server_write) = upstream.into_split();
        // Either side ending drops all four halves: the cut.
        tokio::select! {
            () = pump(client_read, server_write, &self.sent, &self.cut_sent) => {}
            () = pump(server_read, client_write, &self.received, &self.cut_received) => {}
        }
    }
}

/// Copies one direction until it ends or its connection carried `limit` bytes.
async fn pump(
    mut from: tokio::net::tcp::OwnedReadHalf,
    mut to: tokio::net::tcp::OwnedWriteHalf,
    total: &AtomicU64,
    limit: &AtomicU64,
) {
    let mut buffer = vec![0u8; 64 * 1024];
    let mut carried = 0u64;
    loop {
        let read = from.read(&mut buffer).await.unwrap_or(0);
        if read == 0 || to.write_all(&buffer[..read]).await.is_err() {
            return;
        }
        total.fetch_add(read as u64, Ordering::SeqCst);
        carried += read as u64;
        let limit = limit.load(Ordering::SeqCst);
        if limit > 0 && carried >= limit {
            return;
        }
    }
}

/// A login as alice through `proxy`.
struct Login {
    service: SftpService,
    conn_id: String,
    _terminal: MuxedConnectionLease,
    _sandbox: Sandbox,
    proxy: Arc<Proxy>,
}

async fn login_through(proxy: Arc<Proxy>) -> Login {
    let sandbox = Sandbox::new();
    let port = format!("  Port {}", proxy.port);
    sandbox.write_config(&[
        "Host box",
        "  HostName 127.0.0.1",
        "  User alice",
        &port,
        "  PubkeyAuthentication no",
        "  StrictHostKeyChecking accept-new",
    ]);
    let manager = SshConnectionManager::new(sandbox.options());
    let terminal = manager
        .connect(
            &SshProfile::new("box"),
            TestIo::accepting("alice-pass"),
            ConnectionLeaseOwner::Terminal,
            ConnectOptions::default(),
        )
        .await
        .expect("alice signs in");
    Login {
        conn_id: terminal.connection().id.clone(),
        service: SftpService::new(manager),
        _terminal: terminal,
        _sandbox: sandbox,
        proxy,
    }
}

async fn login() -> Login {
    login_through(Proxy::start().await).await
}

impl Login {
    async fn upload(
        &self,
        path: &str,
        options: &UploadOptions,
        body: &[u8],
    ) -> Result<(), Problem> {
        self.service
            .upload(&self.conn_id, path, options, body, &mut |_| {})
            .await
            .map(|_| ())
    }

    async fn read(&self, path: &str) -> Vec<u8> {
        self.service
            .download(&self.conn_id, path)
            .await
            .unwrap()
            .read_all()
            .await
            .unwrap()
    }

    /// The permission bits and size of `name` in `dir`.
    async fn stat(&self, dir: &str, name: &str) -> Option<(u32, u64)> {
        self.service
            .list(&self.conn_id, dir)
            .await
            .unwrap()
            .entries
            .into_iter()
            .find(|entry| entry.name == name)
            .map(|entry| (entry.mode.unwrap_or(0), entry.size.unwrap_or(0)))
    }

    /// A fresh folder under the home directory for one test.
    async fn scratch(&self, name: &str) -> String {
        let dir = format!("/home/alice/muxus-check-{name}");
        let _ = self.service.delete(&self.conn_id, &dir).await;
        self.service.mkdir(&self.conn_id, &dir, true).await.unwrap();
        dir
    }
}

fn deps(
    login: &Login,
) -> SftpUploadDeps<impl Fn(ConfirmRequest) -> BoxFuture<'static, bool> + Send + Sync> {
    SftpUploadDeps {
        service: login.service.clone(),
        confirm: |_request: ConfirmRequest| -> BoxFuture<'static, bool> {
            Box::pin(async { true })
        },
    }
}

/// A local file with `bytes`, as one item of a drop.
fn local_file(dir: &tempfile::TempDir, name: &str, bytes: &[u8]) -> DroppedFile {
    let source = dir.path().join(name);
    std::fs::write(&source, bytes).unwrap();
    DroppedFile {
        source,
        relative_path: name.to_string(),
        size: bytes.len() as u64,
    }
}

async fn drop_upload(
    login: &Login,
    directory: &str,
    file: &DroppedFile,
) -> (
    Result<UploadResult, crate::sftp::upload::UploadError>,
    Vec<u64>,
) {
    let payload = DropPayload {
        files: vec![file.clone()],
        directories: Vec::new(),
    };
    let mut loaded = Vec::new();
    let result = upload_to_remote(
        &login.conn_id,
        directory,
        &payload,
        &CancelSignal::new(),
        &mut |update| loaded.push(update.loaded),
        &deps(login),
    )
    .await;
    (result, loaded)
}

#[tokio::test]
#[ignore = "needs the muxusrs-e2e-sshd container: see the module docs"]
async fn new_files_get_0644_less_the_servers_umask_and_replaced_files_keep_their_mode() {
    let login = login().await;
    let dir = login.scratch("modes").await;
    let new_mode = 0o644 & !umask();

    // A small file goes through a randomly named temporary file.
    login
        .upload(
            &format!("{dir}/small.txt"),
            &UploadOptions::default(),
            b"payload",
        )
        .await
        .unwrap();
    assert_eq!(login.stat(&dir, "small.txt").await, Some((new_mode, 7)));

    // A file of 1 MiB or more goes through the staged file named after it.
    let local = tempfile::tempdir().unwrap();
    let big = random_bytes(MIB + 4096, 11);
    let file = local_file(&local, "big.bin", &big);
    let (result, _) = drop_upload(&login, &dir, &file).await;
    assert_eq!(result.unwrap().uploaded, 1);
    assert_eq!(
        login.stat(&dir, "big.bin").await,
        Some((new_mode, big.len() as u64))
    );
    assert_eq!(
        sha256(&login.read(&format!("{dir}/big.bin")).await),
        sha256(&big)
    );

    // A replaced file keeps the mode it had, whatever the umask.
    let script = format!("{dir}/run.sh");
    login
        .upload(&script, &UploadOptions::default(), b"#!/bin/sh\n")
        .await
        .unwrap();
    run(&login, &format!("chmod 750 {script}")).await;
    let replace = UploadOptions {
        overwrite: true,
        ..UploadOptions::default()
    };
    login
        .upload(&script, &replace, b"#!/bin/sh\necho\n")
        .await
        .unwrap();
    assert_eq!(login.stat(&dir, "run.sh").await, Some((0o750, 15)));

    // An editor save keeps it too.
    login
        .service
        .save_file(
            &login.conn_id,
            &script,
            &crate::sftp::types::SftpFileSaveRequest {
                content: "#!/bin/sh\necho saved\n".into(),
                expected_mtime_ms: None,
                force: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(login.stat(&dir, "run.sh").await, Some((0o750, 21)));
    login.service.delete(&login.conn_id, &dir).await.unwrap();
}

/// Runs a shell command as alice over the test's connection.
async fn run(login: &Login, command: &str) {
    let connection = login
        .service
        .manager()
        .get(&login.conn_id)
        .expect("the connection");
    let output = connection
        .exec(command, Duration::from_secs(20))
        .await
        .unwrap_or_else(|err| panic!("{command}: {err}"));
    assert_eq!(
        output.exit_status,
        Some(0),
        "{command}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "needs the muxusrs-e2e-sshd container: see the module docs"]
async fn continues_an_upload_that_a_dropped_connection_cut_short() {
    let local = tempfile::tempdir().unwrap();
    let source = random_bytes(8 * MIB, 23);
    let file = local_file(&local, "image.bin", &source);
    let described = upload_source(&file).await.unwrap().expect("a source");
    let clean = login().await;
    let dir = clean.scratch("upload").await;
    let staged = staged_name(&format!("{dir}/image.bin"), &described);
    let staged_file = staged.rsplit('/').next().unwrap().to_string();

    // The connection drops after 3 MiB went to the server.
    let proxy = Proxy::start().await;
    proxy.cut_sent.store(3 * MIB as u64, Ordering::SeqCst);
    let cut = login_through(proxy.clone()).await;
    let (failed, _) = tokio::time::timeout(Duration::from_secs(60), drop_upload(&cut, &dir, &file))
        .await
        .expect("the upload ends when its connection drops");
    assert!(failed.is_err());
    drop(cut);

    // What reached the server is still there, under the name made from the local file.
    let (_, kept) = clean
        .stat(&dir, &staged_file)
        .await
        .expect("the staged file stays");
    assert!(
        kept > 0 && (kept as usize) < source.len(),
        "{kept} bytes kept"
    );
    assert_eq!(clean.stat(&dir, "image.bin").await, None);

    // The next upload, on a new connection to the same host, sends only the rest.
    proxy.cut_sent.store(0, Ordering::SeqCst);
    let next = login_through(proxy).await;
    let before = next.proxy.sent.load(Ordering::SeqCst);
    let (result, loaded) = drop_upload(&next, &dir, &file).await;
    assert_eq!(result.unwrap().uploaded, 1);
    let sent = next.proxy.sent.load(Ordering::SeqCst) - before;
    let rest = source.len() as u64 - kept;
    assert!(
        sent >= rest && sent < rest + MIB as u64,
        "{sent} bytes sent for the remaining {rest}"
    );
    // The bar starts where the first attempt stopped.
    assert!(loaded.contains(&kept), "{kept} in {loaded:?}");
    assert_eq!(
        clean.stat(&dir, "image.bin").await,
        Some((0o644 & !umask(), source.len() as u64))
    );
    assert_eq!(clean.stat(&dir, &staged_file).await, None);
    assert_eq!(
        sha256(&clean.read(&format!("{dir}/image.bin")).await),
        sha256(&source)
    );
    clean.service.delete(&clean.conn_id, &dir).await.unwrap();
}

#[tokio::test]
#[ignore = "needs the muxusrs-e2e-sshd container: see the module docs"]
async fn sends_a_file_whole_again_when_what_the_server_kept_is_not_its_own() {
    let local = tempfile::tempdir().unwrap();
    let source = random_bytes(2 * MIB, 37);
    let file = local_file(&local, "image.bin", &source);
    let described = upload_source(&file).await.unwrap().expect("a source");
    let login = login().await;
    let dir = login.scratch("mismatch").await;
    let staged = staged_name(&format!("{dir}/image.bin"), &described);

    // A look-alike under the staged name: the same start, another megabyte after it.
    let mut other = source[..MIB / 2].to_vec();
    other.extend(random_bytes(MIB / 2, 41));
    login
        .upload(&staged, &UploadOptions::default(), &other)
        .await
        .unwrap();

    // Asked to continue it, the server refuses and removes it.
    let refused = login
        .service
        .upload(
            &login.conn_id,
            &format!("{dir}/image.bin"),
            &UploadOptions {
                source: Some(described),
                ..UploadOptions::default()
            },
            &source[..],
            &mut |_: UploadStep| {},
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code.as_deref(), Some("SFTP_UPLOAD_MISMATCH"));
    assert_eq!(login.stat(&dir, "image.bin").await, None);
    assert_eq!(
        login.stat(&dir, staged.rsplit('/').next().unwrap()).await,
        None
    );

    // The upload as the file browser runs it sends the file again, whole.
    login
        .upload(&staged, &UploadOptions::default(), &other)
        .await
        .unwrap();
    let (result, _) = drop_upload(&login, &dir, &file).await;
    assert_eq!(result.unwrap().uploaded, 1);
    assert_eq!(
        sha256(&login.read(&format!("{dir}/image.bin")).await),
        sha256(&source)
    );
    login.service.delete(&login.conn_id, &dir).await.unwrap();
}

#[tokio::test]
#[ignore = "needs the muxusrs-e2e-sshd container: see the module docs"]
async fn continues_a_download_that_a_dropped_connection_cut_short() {
    let source = random_bytes(8 * MIB, 53);
    let clean = login().await;
    let dir = clean.scratch("download").await;
    let path = format!("{dir}/image.bin");
    clean
        .upload(&path, &UploadOptions::default(), &source)
        .await
        .unwrap();

    // The connection drops after 3 MiB came from the server.
    let proxy = Proxy::start().await;
    proxy.cut_received.store(3 * MIB as u64, Ordering::SeqCst);
    let cut = login_through(proxy.clone()).await;
    let mut download = cut.service.download(&cut.conn_id, &path).await.unwrap();
    let etag = download.etag.clone().expect("a validator");
    let mut received = Vec::new();
    let ended = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match download.next_chunk().await {
                Ok(Some(chunk)) => received.extend_from_slice(&chunk),
                other => break other,
            }
        }
    })
    .await
    .expect("the download ends when its connection drops");
    assert!(ended.is_err(), "{ended:?}");
    assert!(!received.is_empty() && received.len() < source.len());
    assert_eq!(received, source[..received.len()]);
    drop(cut);

    // The next download, on another connection to the same host, asks only for the rest.
    proxy.cut_received.store(0, Ordering::SeqCst);
    let next = login_through(proxy).await;
    let before = next.proxy.received.load(Ordering::SeqCst);
    let range = DownloadRange {
        start: received.len() as u64,
        etag,
    };
    let rest = next
        .service
        .download_from(&next.conn_id, &path, Some(&range))
        .await
        .unwrap();
    assert_eq!(rest.start, range.start);
    assert_eq!(rest.etag.as_deref(), Some(range.etag.as_str()));
    received.extend(rest.read_all().await.unwrap());
    let came = next.proxy.received.load(Ordering::SeqCst) - before;
    let asked = source.len() as u64 - range.start;
    assert!(
        came >= asked && came < asked + MIB as u64,
        "{came} bytes came for the remaining {asked}"
    );
    assert_eq!(sha256(&received), sha256(&source));

    // Changed on the server, the file comes whole.
    run(&next, &format!("touch -d '2001-02-03 04:05:06' {path}")).await;
    let changed = next
        .service
        .download_from(&next.conn_id, &path, Some(&range))
        .await
        .unwrap();
    assert_eq!(changed.start, 0);
    assert_ne!(changed.etag.as_deref(), Some(range.etag.as_str()));
    drop(changed);
    // The same file reached another way is another host's: it comes whole too.
    let elsewhere = login().await;
    let other_host = elsewhere
        .service
        .download(&elsewhere.conn_id, &path)
        .await
        .unwrap();
    assert_ne!(
        other_host.etag,
        next.service
            .download(&next.conn_id, &path)
            .await
            .unwrap()
            .etag
    );
    drop(other_host);
    clean.service.delete(&clean.conn_id, &dir).await.unwrap();
}
