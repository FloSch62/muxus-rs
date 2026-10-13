//! An SFTP subsystem for the test server, backed by a local directory.
//!
//! Remote paths resolve below `root` (`/srv/a` is `<root>/srv/a`; relative paths start at the
//! configured home). Plain `rename` refuses an existing destination the way OpenSSH's SFTP v3
//! rename does, and `posix-rename@openssh.com` replaces atomically when it is enabled. Every
//! request except reads, writes and closes lands in a per-root log the tests assert on.

use std::collections::HashMap;
use std::os::unix::fs::{FileExt, MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, UNIX_EPOCH};

use russh_sftp::protocol::{
    Attrs, Data, File, FileAttributes, Handle, Name, OpenFlags, Packet, Status, StatusCode, Version,
};
use russh_sftp::server::StatusReply;

const POSIX_RENAME: &str = "posix-rename@openssh.com";

/// How a served root behaves. Register it with [`register`] before the client opens SFTP.
#[derive(Clone, Debug)]
pub struct SftpServerOptions {
    /// Advertise and implement `posix-rename@openssh.com`, as OpenSSH does.
    pub posix_rename: bool,
    /// What `realpath(".")` answers; relative paths start here.
    pub home: String,
    /// Owner and group names shown in directory long names.
    pub owner: String,
    pub group: String,
}

impl Default for SftpServerOptions {
    fn default() -> Self {
        Self {
            posix_rename: true,
            home: "/".into(),
            owner: "tester".into(),
            group: "staff".into(),
        }
    }
}

/// A served root's options and request log.
#[derive(Default)]
pub struct SftpServerState {
    pub options: Mutex<SftpServerOptions>,
    pub log: Mutex<Vec<String>>,
}

impl SftpServerState {
    /// Requests seen so far, e.g. `open /a.txt wctx 644` or `posix-rename /a.tmp /a`.
    pub fn ops(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn record(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
}

fn registry() -> &'static Mutex<HashMap<PathBuf, Arc<SftpServerState>>> {
    static REGISTRY: OnceLock<Mutex<HashMap<PathBuf, Arc<SftpServerState>>>> = OnceLock::new();
    REGISTRY.get_or_init(Default::default)
}

/// Sets how `root` is served and returns its state (log included).
pub fn register(root: &Path, options: SftpServerOptions) -> Arc<SftpServerState> {
    let state = Arc::new(SftpServerState {
        options: Mutex::new(options),
        log: Mutex::new(Vec::new()),
    });
    registry()
        .lock()
        .unwrap()
        .insert(root.to_path_buf(), state.clone());
    state
}

/// The state of `root`; default options when nothing was registered.
pub fn state(root: &Path) -> Arc<SftpServerState> {
    registry()
        .lock()
        .unwrap()
        .entry(root.to_path_buf())
        .or_default()
        .clone()
}

/// Serves SFTP on `stream` with `root` as the file system root.
pub async fn serve<S>(stream: S, root: PathBuf)
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let state = state(&root);
    let handler = LocalFs {
        root,
        state,
        handles: HashMap::new(),
        next_handle: 0,
    };
    russh_sftp::server::run(stream, handler).await;
}

enum OpenHandle {
    File(std::fs::File),
    Dir { entries: Vec<File>, sent: bool },
}

struct LocalFs {
    root: PathBuf,
    state: Arc<SftpServerState>,
    handles: HashMap<String, OpenHandle>,
    next_handle: u64,
}

fn status_message(code: StatusCode) -> &'static str {
    match code {
        StatusCode::Ok => "Success",
        StatusCode::Eof => "End of file",
        StatusCode::NoSuchFile => "No such file",
        StatusCode::PermissionDenied => "Permission denied",
        StatusCode::BadMessage => "Bad message",
        StatusCode::OpUnsupported => "Operation unsupported",
        _ => "Failure",
    }
}

fn reply(code: StatusCode) -> StatusReply {
    code.with_message(status_message(code))
}

/// errno → status, as OpenSSH's `errno_to_portable` does.
fn io_reply(err: std::io::Error) -> StatusReply {
    reply(match err.kind() {
        std::io::ErrorKind::NotFound => StatusCode::NoSuchFile,
        std::io::ErrorKind::PermissionDenied => StatusCode::PermissionDenied,
        _ => StatusCode::Failure,
    })
}

fn ok(id: u32) -> Status {
    Status {
        id,
        status_code: StatusCode::Ok,
        error_message: "Success".into(),
        language_tag: "en-US".into(),
    }
}

fn attrs_of(meta: &std::fs::Metadata) -> FileAttributes {
    FileAttributes {
        size: Some(meta.size()),
        uid: Some(meta.uid()),
        user: None,
        gid: Some(meta.gid()),
        group: None,
        permissions: Some(meta.mode()),
        atime: Some(meta.atime() as u32),
        mtime: Some(meta.mtime() as u32),
    }
}

/// `r`, `w`, `a`, `c`, `t`, `x` for READ, WRITE, APPEND, CREATE, TRUNCATE, EXCLUDE.
fn flag_letters(flags: OpenFlags) -> String {
    [
        (OpenFlags::READ, 'r'),
        (OpenFlags::WRITE, 'w'),
        (OpenFlags::APPEND, 'a'),
        (OpenFlags::CREATE, 'c'),
        (OpenFlags::TRUNCATE, 't'),
        (OpenFlags::EXCLUDE, 'x'),
    ]
    .iter()
    .filter(|(flag, _)| flags.contains(*flag))
    .map(|(_, letter)| *letter)
    .collect()
}

/// Lexically normalized absolute remote path.
fn normalize(path: &str, home: &str) -> String {
    let joined = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("{home}/{path}")
    };
    let mut parts: Vec<&str> = Vec::new();
    for part in joined.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            other => parts.push(other),
        }
    }
    format!("/{}", parts.join("/"))
}

fn read_string(data: &[u8], offset: &mut usize) -> Option<String> {
    let len_bytes = data.get(*offset..*offset + 4)?;
    let len = u32::from_be_bytes(len_bytes.try_into().ok()?) as usize;
    let start = *offset + 4;
    let bytes = data.get(start..start + len)?;
    *offset = start + len;
    String::from_utf8(bytes.to_vec()).ok()
}

impl LocalFs {
    fn remote(&self, path: &str) -> String {
        let home = self.state.options.lock().unwrap().home.clone();
        normalize(path, &home)
    }

    fn local(&self, path: &str) -> PathBuf {
        let remote = self.remote(path);
        self.root.join(remote.trim_start_matches('/'))
    }

    fn record(&self, entry: String) {
        self.state.record(entry);
    }

    fn add_handle(&mut self, handle: OpenHandle) -> String {
        self.next_handle += 1;
        let id = format!("h{}", self.next_handle);
        self.handles.insert(id.clone(), handle);
        id
    }

    fn file(&self, handle: &str) -> Result<&std::fs::File, StatusReply> {
        match self.handles.get(handle) {
            Some(OpenHandle::File(file)) => Ok(file),
            _ => Err(reply(StatusCode::Failure)),
        }
    }

    fn set_attrs(&self, path: &Path, attrs: &FileAttributes) -> Result<(), StatusReply> {
        if let Some(mode) = attrs.permissions {
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o7777))
                .map_err(io_reply)?;
        }
        if let Some(mtime) = attrs.mtime {
            let file = std::fs::File::open(path).map_err(io_reply)?;
            file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(mtime)))
                .map_err(io_reply)?;
        }
        Ok(())
    }

    fn long_entry(&self, name: &str, meta: &std::fs::Metadata) -> File {
        let options = self.state.options.lock().unwrap().clone();
        let mut attrs = attrs_of(meta);
        attrs.user = Some(options.owner);
        attrs.group = Some(options.group);
        let mut entry = File::new(name, attrs);
        entry.attrs.user = None;
        entry.attrs.group = None;
        entry
    }
}

impl russh_sftp::server::Handler for LocalFs {
    type Error = StatusReply;

    fn unimplemented(&self) -> Self::Error {
        reply(StatusCode::OpUnsupported)
    }

    async fn init(
        &mut self,
        _version: u32,
        _extensions: HashMap<String, String>,
    ) -> Result<Version, Self::Error> {
        let mut version = Version::new();
        if self.state.options.lock().unwrap().posix_rename {
            version.extensions.insert(POSIX_RENAME.into(), "1".into());
        }
        Ok(version)
    }

    async fn open(
        &mut self,
        id: u32,
        filename: String,
        pflags: OpenFlags,
        attrs: FileAttributes,
    ) -> Result<Handle, Self::Error> {
        let mode = attrs
            .permissions
            .map_or("-".to_string(), |mode| format!("{:o}", mode & 0o7777));
        self.record(format!(
            "open {} {} {mode}",
            self.remote(&filename),
            flag_letters(pflags)
        ));
        let mut options: std::fs::OpenOptions = pflags.into();
        if let Some(mode) = attrs.permissions {
            options.mode(mode & 0o7777);
        }
        let file = options.open(self.local(&filename)).map_err(|err| {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                reply(StatusCode::Failure)
            } else {
                io_reply(err)
            }
        })?;
        let handle = self.add_handle(OpenHandle::File(file));
        Ok(Handle { id, handle })
    }

    async fn close(&mut self, id: u32, handle: String) -> Result<Status, Self::Error> {
        match self.handles.remove(&handle) {
            Some(_) => Ok(ok(id)),
            None => Err(reply(StatusCode::Failure)),
        }
    }

    async fn read(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        len: u32,
    ) -> Result<Data, Self::Error> {
        let file = self.file(&handle)?;
        let mut data = vec![0u8; len as usize];
        let mut filled = 0;
        while filled < data.len() {
            match file.read_at(&mut data[filled..], offset + filled as u64) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(err) => return Err(io_reply(err)),
            }
        }
        if filled == 0 && len > 0 {
            return Err(reply(StatusCode::Eof));
        }
        data.truncate(filled);
        Ok(Data { id, data })
    }

    async fn write(
        &mut self,
        id: u32,
        handle: String,
        offset: u64,
        data: Vec<u8>,
    ) -> Result<Status, Self::Error> {
        let file = self.file(&handle)?;
        file.write_all_at(&data, offset).map_err(io_reply)?;
        Ok(ok(id))
    }

    async fn lstat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.record(format!("lstat {}", self.remote(&path)));
        let meta = std::fs::symlink_metadata(self.local(&path)).map_err(io_reply)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn stat(&mut self, id: u32, path: String) -> Result<Attrs, Self::Error> {
        self.record(format!("stat {}", self.remote(&path)));
        let meta = std::fs::metadata(self.local(&path)).map_err(io_reply)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn fstat(&mut self, id: u32, handle: String) -> Result<Attrs, Self::Error> {
        let meta = self.file(&handle)?.metadata().map_err(io_reply)?;
        Ok(Attrs {
            id,
            attrs: attrs_of(&meta),
        })
    }

    async fn setstat(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.record(format!("setstat {}", self.remote(&path)));
        self.set_attrs(&self.local(&path), &attrs)?;
        Ok(ok(id))
    }

    async fn fsetstat(
        &mut self,
        id: u32,
        handle: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        let file = self.file(&handle)?;
        if let Some(mode) = attrs.permissions {
            file.set_permissions(std::fs::Permissions::from_mode(mode & 0o7777))
                .map_err(io_reply)?;
        }
        if let Some(mtime) = attrs.mtime {
            file.set_modified(UNIX_EPOCH + Duration::from_secs(u64::from(mtime)))
                .map_err(io_reply)?;
        }
        Ok(ok(id))
    }

    async fn opendir(&mut self, id: u32, path: String) -> Result<Handle, Self::Error> {
        self.record(format!("opendir {}", self.remote(&path)));
        let dir = self.local(&path);
        let meta = std::fs::metadata(&dir).map_err(io_reply)?;
        if !meta.is_dir() {
            return Err(reply(StatusCode::Failure));
        }
        let mut entries = vec![self.long_entry(".", &meta), self.long_entry("..", &meta)];
        let mut children: Vec<_> = std::fs::read_dir(&dir)
            .map_err(io_reply)?
            .filter_map(Result::ok)
            .collect();
        children.sort_by_key(|entry| entry.file_name());
        for child in children {
            let Ok(meta) = std::fs::symlink_metadata(child.path()) else {
                continue;
            };
            entries.push(self.long_entry(&child.file_name().to_string_lossy(), &meta));
        }
        let handle = self.add_handle(OpenHandle::Dir {
            entries,
            sent: false,
        });
        Ok(Handle { id, handle })
    }

    async fn readdir(&mut self, id: u32, handle: String) -> Result<Name, Self::Error> {
        match self.handles.get_mut(&handle) {
            Some(OpenHandle::Dir { entries, sent }) if !*sent => {
                *sent = true;
                Ok(Name {
                    id,
                    files: std::mem::take(entries),
                })
            }
            Some(OpenHandle::Dir { .. }) => Err(reply(StatusCode::Eof)),
            _ => Err(reply(StatusCode::Failure)),
        }
    }

    async fn remove(&mut self, id: u32, filename: String) -> Result<Status, Self::Error> {
        self.record(format!("remove {}", self.remote(&filename)));
        let path = self.local(&filename);
        let meta = std::fs::symlink_metadata(&path).map_err(io_reply)?;
        if meta.is_dir() {
            return Err(reply(StatusCode::Failure));
        }
        std::fs::remove_file(path).map_err(io_reply)?;
        Ok(ok(id))
    }

    async fn mkdir(
        &mut self,
        id: u32,
        path: String,
        attrs: FileAttributes,
    ) -> Result<Status, Self::Error> {
        self.record(format!("mkdir {}", self.remote(&path)));
        let mut builder = std::fs::DirBuilder::new();
        if let Some(mode) = attrs.permissions {
            std::os::unix::fs::DirBuilderExt::mode(&mut builder, mode & 0o7777);
        }
        builder.create(self.local(&path)).map_err(|err| {
            if err.kind() == std::io::ErrorKind::AlreadyExists {
                reply(StatusCode::Failure)
            } else {
                io_reply(err)
            }
        })?;
        Ok(ok(id))
    }

    async fn rmdir(&mut self, id: u32, path: String) -> Result<Status, Self::Error> {
        self.record(format!("rmdir {}", self.remote(&path)));
        std::fs::remove_dir(self.local(&path)).map_err(io_reply)?;
        Ok(ok(id))
    }

    async fn realpath(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        self.record(format!("realpath {path}"));
        Ok(Name {
            id,
            files: vec![File::dummy(self.remote(&path))],
        })
    }

    async fn rename(
        &mut self,
        id: u32,
        oldpath: String,
        newpath: String,
    ) -> Result<Status, Self::Error> {
        self.record(format!(
            "rename {} {}",
            self.remote(&oldpath),
            self.remote(&newpath)
        ));
        let (from, to) = (self.local(&oldpath), self.local(&newpath));
        std::fs::symlink_metadata(&from).map_err(io_reply)?;
        // SFTP v3 rename never replaces an existing destination.
        if std::fs::symlink_metadata(&to).is_ok() {
            return Err(reply(StatusCode::Failure));
        }
        std::fs::rename(from, to).map_err(io_reply)?;
        Ok(ok(id))
    }

    async fn readlink(&mut self, id: u32, path: String) -> Result<Name, Self::Error> {
        let target = std::fs::read_link(self.local(&path)).map_err(io_reply)?;
        Ok(Name {
            id,
            files: vec![File::dummy(target.to_string_lossy())],
        })
    }

    async fn symlink(
        &mut self,
        id: u32,
        linkpath: String,
        targetpath: String,
    ) -> Result<Status, Self::Error> {
        self.record(format!("symlink {} {targetpath}", self.remote(&linkpath)));
        std::os::unix::fs::symlink(targetpath, self.local(&linkpath)).map_err(io_reply)?;
        Ok(ok(id))
    }

    async fn extended(
        &mut self,
        id: u32,
        request: String,
        data: Vec<u8>,
    ) -> Result<Packet, Self::Error> {
        if request != POSIX_RENAME || !self.state.options.lock().unwrap().posix_rename {
            self.record(format!("extended {request}"));
            return Err(reply(StatusCode::OpUnsupported));
        }
        let mut offset = 0;
        let (Some(oldpath), Some(newpath)) = (
            read_string(&data, &mut offset),
            read_string(&data, &mut offset),
        ) else {
            return Err(reply(StatusCode::BadMessage));
        };
        self.record(format!(
            "posix-rename {} {}",
            self.remote(&oldpath),
            self.remote(&newpath)
        ));
        std::fs::rename(self.local(&oldpath), self.local(&newpath)).map_err(io_reply)?;
        Ok(Packet::Status(ok(id)))
    }
}

#[cfg(test)]
mod tests {
    use super::normalize;

    #[test]
    fn resolves_remote_paths_lexically_below_the_root() {
        assert_eq!(normalize(".", "/home/tester"), "/home/tester");
        assert_eq!(
            normalize("docs/../a.txt", "/home/tester"),
            "/home/tester/a.txt"
        );
        assert_eq!(normalize("/../../etc", "/"), "/etc");
        assert_eq!(normalize("/srv//app/", "/"), "/srv/app");
    }
}
