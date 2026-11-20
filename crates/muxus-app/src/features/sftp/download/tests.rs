//! Port of `tests/unit/client/sftp-download.test.ts` and the download cases of
//! `tests/unit/client/transfers.test.ts`.

use std::cell::{Cell, RefCell};
use std::collections::VecDeque;

use futures_lite::future::block_on;

use super::*;

// --- saving downloads into a folder -----------------------------------------------------------

#[test]
fn writes_the_file_under_its_own_name() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(
        save_into_folder(folder.path(), "report.txt", b"payload").unwrap(),
        "report.txt"
    );
    assert_eq!(
        std::fs::read(folder.path().join("report.txt")).unwrap(),
        b"payload"
    );
}

#[test]
fn numbers_a_name_that_is_taken_instead_of_replacing_what_is_there() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::write(folder.path().join("report.txt"), "old").unwrap();
    std::fs::create_dir(folder.path().join("report (1).txt")).unwrap();
    assert_eq!(
        save_into_folder(folder.path(), "report.txt", b"new").unwrap(),
        "report (2).txt"
    );
    assert_eq!(
        std::fs::read_to_string(folder.path().join("report.txt")).unwrap(),
        "old"
    );
    assert_eq!(
        std::fs::read_to_string(folder.path().join("report (2).txt")).unwrap(),
        "new"
    );
}

#[test]
fn cleans_remote_names_that_local_file_systems_refuse() {
    let folder = tempfile::tempdir().unwrap();
    assert_eq!(
        save_into_folder(folder.path(), "show run: 10/24?.txt", b"x").unwrap(),
        "show run_ 10_24_.txt"
    );
    assert!(folder.path().join("show run_ 10_24_.txt").is_file());
}

#[test]
fn numbers_a_folder_whose_name_is_taken_and_cleans_its_name() {
    let folder = tempfile::tempdir().unwrap();
    std::fs::create_dir(folder.path().join("logs")).unwrap();
    std::fs::write(folder.path().join("logs (1)"), "a file").unwrap();

    let created = create_folder_in(folder.path(), "logs").unwrap();
    assert_eq!(created, folder.path().join("logs (2)"));
    assert!(created.is_dir());
    // A dot in a folder's name is no extension: the number goes after it.
    std::fs::create_dir(folder.path().join("v1.2")).unwrap();
    assert_eq!(
        create_folder_in(folder.path(), "v1.2").unwrap(),
        folder.path().join("v1.2 (1)")
    );
    assert_eq!(
        create_folder_in(folder.path(), "2024: q1?").unwrap(),
        folder.path().join("2024_ q1_")
    );
}

// --- download file names ----------------------------------------------------------------------

#[test]
fn numbers_before_the_extension_and_after_a_dotfile() {
    assert_eq!(numbered_file_name("report.txt", 0), "report.txt");
    assert_eq!(numbered_file_name("report.txt", 3), "report (3).txt");
    assert_eq!(numbered_file_name(".bashrc", 1), ".bashrc (1)");
    assert_eq!(numbered_file_name("Makefile", 1), "Makefile (1)");
}

#[test]
fn replaces_separators_reserved_and_control_characters_and_trailing_dots() {
    assert_eq!(local_file_name("a\\b|c*\"d\"<e>"), "a_b_c__d__e_");
    assert_eq!(local_file_name("tab\there"), "tab_here");
    assert_eq!(local_file_name("notes. "), "notes");
    assert_eq!(local_file_name(".."), "download");
}

#[test]
fn a_hostile_remote_name_stays_in_the_downloads_folder() {
    let folder = tempfile::tempdir().unwrap();
    for name in ["../../.bashrc", "/etc/passwd", "..", "a/../../b", "..\\x"] {
        let path = crate::util::file_dialog::free_path(folder.path(), &local_file_name(name));
        assert_eq!(path.parent(), Some(folder.path()), "{name}");
    }
}

#[test]
fn knows_the_names_windows_keeps_for_devices() {
    for name in [
        "aux.c",
        "con",
        "CON.txt",
        "Nul",
        "prn.tar.gz",
        "com1",
        "LPT9.log",
        "clock$",
        "con .txt",
    ] {
        assert!(reserved_on_windows(name), "{name}");
    }
    for name in [
        "console",
        "auxiliary.c",
        "com10",
        "com0",
        "lpt",
        "desktop.ini",
        "x.lnk",
        "main.c",
        ".aux",
    ] {
        assert!(!reserved_on_windows(name), "{name}");
    }
    assert!(is_refused_name(&io::Error::new(
        io::ErrorKind::InvalidFilename,
        "name not allowed locally"
    )));
    assert!(!is_refused_name(&io::Error::from(
        io::ErrorKind::PermissionDenied
    )));
}

// --- continuing interrupted downloads ---------------------------------------------------------

fn store() -> SharedInterruptedDownloads {
    SharedInterruptedDownloads::default()
}

fn partial(text: &str) -> PartialDownload {
    PartialDownload {
        bytes: text.as_bytes().to_vec(),
        etag: "\"v1\"".into(),
    }
}

/// What the next download of `file` would continue from.
fn kept(store: &SharedInterruptedDownloads, file: &str) -> Option<String> {
    store
        .borrow()
        .kept(file)
        .map(|partial| String::from_utf8_lossy(&partial.bytes).into_owned())
}

/// A download of `file` that broke off after `text`, for a reason other than Cancel.
fn keep(store: &SharedInterruptedDownloads, file: &str, text: &str) {
    DownloadResume::new(file, store, &TransferStop::new()).interrupted(partial(text));
}

#[test]
fn offers_what_a_broken_download_received_to_the_next_download_of_the_file() {
    let store = store();
    let stop = TransferStop::new();
    let mut first = DownloadResume::new("/srv/a.iso", &store, &stop);
    assert_eq!(first.range(), None);
    first.interrupted(partial("abc"));
    drop(first);

    let next = DownloadResume::new("/srv/a.iso", &store, &stop);
    assert_eq!(
        next.range(),
        Some(DownloadRange {
            start: 3,
            etag: "\"v1\"".into()
        })
    );
    assert_eq!(
        DownloadResume::new("/srv/other.iso", &store, &stop).range(),
        None
    );
    drop(next);
    assert_eq!(kept(&store, "/srv/a.iso").as_deref(), Some("abc"));

    store.borrow_mut().forget("/srv/a.iso");
    assert_eq!(
        DownloadResume::new("/srv/a.iso", &store, &stop).range(),
        None
    );
}

#[test]
fn keeps_a_download_stopped_by_the_file_browser_closing_and_drops_a_cancelled_one() {
    let store = store();
    let closing = TransferStop::new();
    let mut closed = DownloadResume::new("/srv/a.iso", &store, &closing);
    closing.stop(StopReason::Closed);
    closed.interrupted(partial("abc"));
    drop(closed);
    assert_eq!(kept(&store, "/srv/a.iso").as_deref(), Some("abc"));

    let cancelling = TransferStop::new();
    let mut cancelled = DownloadResume::new("/srv/a.iso", &store, &cancelling);
    cancelling.stop(StopReason::Cancelled);
    cancelled.interrupted(partial("abcdef"));
    drop(cancelled);
    assert_eq!(kept(&store, "/srv/a.iso"), None);
}

#[test]
fn drops_what_was_kept_when_cancel_comes_before_the_server_answers() {
    let store = store();
    keep(&store, "/srv/a.iso", "abc");

    let cancelling = TransferStop::new();
    let waiting = DownloadResume::new("/srv/a.iso", &store, &cancelling);
    cancelling.stop(StopReason::Cancelled);
    drop(waiting);

    assert_eq!(kept(&store, "/srv/a.iso"), None);
}

#[test]
fn leaves_the_file_alone_on_a_cancel_that_comes_after_it_was_released() {
    let store = store();
    let cancelling = TransferStop::new();
    let mut finished = DownloadResume::new("/srv/a.iso", &store, &cancelling);
    finished.forget();
    drop(finished);
    keep(&store, "/srv/a.iso", "abc");
    cancelling.stop(StopReason::Cancelled);

    assert_eq!(kept(&store, "/srv/a.iso").as_deref(), Some("abc"));
}

#[test]
fn keeps_the_newest_within_the_limit_and_nothing_larger() {
    // The limit is 256 MiB; the rule is the same at 256 bytes.
    let store: SharedInterruptedDownloads =
        Rc::new(RefCell::new(InterruptedDownloads::with_limit(256)));
    let sized = |size: usize| PartialDownload {
        bytes: vec![0; size],
        etag: "\"v1\"".into(),
    };
    for file in ["/srv/1.iso", "/srv/2.iso", "/srv/3.iso"] {
        store.borrow_mut().keep(file, sized(100));
    }
    store.borrow_mut().keep("/srv/huge.iso", sized(257));

    let has = |file: &str| store.borrow().kept(file).is_some();
    assert_eq!(
        ["/srv/1.iso", "/srv/2.iso", "/srv/3.iso", "/srv/huge.iso"].map(has),
        [false, true, true, false]
    );
    assert_eq!(InterruptedDownloads::default().limit, 256 * 1024 * 1024);
}

// --- streaming one download -------------------------------------------------------------------

/// The answer of `/download`: where it starts, and its chunks.
struct Answer {
    start: u64,
    size: Option<u64>,
    etag: Option<String>,
    chunks: VecDeque<ApiResult<Vec<u8>>>,
    /// Once the chunks are out, run this and wait for ever, as a stalled connection does.
    stalls: Option<Box<dyn FnOnce()>>,
}

impl Answer {
    fn new(start: u64, size: u64, etag: Option<&str>, chunks: &[&str]) -> Self {
        Self {
            start,
            size: Some(size),
            etag: etag.map(str::to_string),
            chunks: chunks
                .iter()
                .map(|chunk| Ok(chunk.as_bytes().to_vec()))
                .collect(),
            stalls: None,
        }
    }
}

impl DownloadStream for Answer {
    fn start(&self) -> u64 {
        self.start
    }

    fn size(&self) -> Option<u64> {
        self.size
    }

    fn etag(&self) -> Option<String> {
        self.etag.clone()
    }

    fn mtime_ms(&self) -> Option<i64> {
        Some(1_760_000_000_000)
    }

    async fn next_chunk(&mut self) -> Option<ApiResult<Vec<u8>>> {
        match (self.chunks.pop_front(), self.stalls.take()) {
            (Some(chunk), stalls) => {
                self.stalls = stalls;
                Some(chunk)
            }
            (None, Some(stalled)) => {
                stalled();
                std::future::pending().await
            }
            (None, None) => None,
        }
    }
}

/// One download of `/srv/a.iso` answered with `answer`: the result, the range asked for, and
/// the progress reported.
#[allow(clippy::type_complexity)]
fn fetch(
    store: &SharedInterruptedDownloads,
    stop: &TransferStop,
    answer: Answer,
) -> (
    ApiResult<Vec<u8>>,
    Option<DownloadRange>,
    Vec<(u64, Option<u64>)>,
) {
    let asked = RefCell::new(None);
    let mut progress = Vec::new();
    let mut resume = DownloadResume::new("/srv/a.iso", store, stop);
    let result = block_on(download_with_progress(
        async |range| {
            *asked.borrow_mut() = range;
            Ok(answer)
        },
        &mut resume,
        stop,
        |_| {},
        &mut |update| progress.push((update.loaded, update.total)),
    ));
    drop(resume);
    (result, asked.into_inner(), progress)
}

#[test]
fn asks_for_the_rest_of_an_interrupted_download_and_adds_it_to_what_was_received() {
    let store = store();
    keep(&store, "/srv/a.iso", "pay");

    let (result, asked, progress) = fetch(
        &store,
        &TransferStop::new(),
        Answer::new(3, 7, Some("\"v1\""), &["load"]),
    );

    assert_eq!(result.unwrap(), b"payload");
    assert_eq!(
        asked,
        Some(DownloadRange {
            start: 3,
            etag: "\"v1\"".into()
        })
    );
    assert_eq!(progress.first(), Some(&(3, Some(7))));
    assert_eq!(progress.last(), Some(&(7, Some(7))));
    assert_eq!(kept(&store, "/srv/a.iso"), None);
}

#[test]
fn takes_the_whole_file_when_it_changed_since_the_interrupted_download() {
    let store = store();
    keep(&store, "/srv/a.iso", "pay");

    let (result, _, progress) = fetch(
        &store,
        &TransferStop::new(),
        Answer::new(0, 7, Some("\"v2\""), &["changed"]),
    );

    assert_eq!(result.unwrap(), b"changed");
    assert_eq!(progress.first(), Some(&(0, Some(7))));
    assert_eq!(kept(&store, "/srv/a.iso"), None);
}

#[test]
fn hands_over_what_it_received_when_the_download_breaks_off() {
    let store = store();
    let mut answer = Answer::new(0, 7, Some("\"v1\""), &["pay"]);
    answer
        .chunks
        .push_back(Err(ApiError::internal("network error")));

    let (result, asked, _) = fetch(&store, &TransferStop::new(), answer);

    assert_eq!(result.unwrap_err().message, "network error");
    assert_eq!(asked, None);
    assert_eq!(
        store.borrow().kept("/srv/a.iso"),
        Some(&PartialDownload {
            bytes: b"pay".to_vec(),
            etag: "\"v1\"".into()
        })
    );
}

#[test]
fn keeps_nothing_of_a_file_the_server_gave_no_validator_for() {
    let store = store();
    let mut answer = Answer::new(0, 7, None, &["pay"]);
    answer
        .chunks
        .push_back(Err(ApiError::internal("network error")));

    let (result, _, _) = fetch(&store, &TransferStop::new(), answer);

    assert!(result.is_err());
    assert_eq!(kept(&store, "/srv/a.iso"), None);
}

#[test]
fn stops_a_stalled_download_and_keeps_what_it_received_unless_cancelled() {
    for (reason, left) in [
        (StopReason::Closed, Some("pay")),
        (StopReason::ConnectionLost, Some("pay")),
        (StopReason::Cancelled, None),
    ] {
        let store = store();
        let stop = TransferStop::new();
        let mut answer = Answer::new(0, 7, Some("\"v1\""), &["pay"]);
        answer.stalls = Some(Box::new({
            let stop = stop.clone();
            move || stop.stop(reason)
        }));

        let (result, _, _) = fetch(&store, &stop, answer);

        let error = result.unwrap_err();
        assert!(super::super::transfer::is_abort_error(&error), "{reason:?}");
        assert_eq!(kept(&store, "/srv/a.iso").as_deref(), left, "{reason:?}");
    }
}

#[test]
fn asks_nothing_of_the_server_once_stopped() {
    let store = store();
    let stop = TransferStop::new();
    stop.stop(StopReason::Cancelled);
    let asked = Cell::new(false);
    let mut resume = DownloadResume::new("/srv/a.iso", &store, &stop);

    let result = block_on(download_with_progress(
        async |_| {
            asked.set(true);
            Ok(Answer::new(0, 7, None, &["payload"]))
        },
        &mut resume,
        &stop,
        |_| {},
        &mut |_| {},
    ));

    assert!(result.is_err());
    assert!(!asked.get());
}

// --- downloading folders ----------------------------------------------------------------------

type RemoteEntry = (&'static str, SftpEntryType, Option<u64>);

const FILE: SftpEntryType = SftpEntryType::File;
const DIR: SftpEntryType = SftpEntryType::Dir;

fn entry(folder: &str, (name, kind, size): RemoteEntry) -> SftpEntry {
    SftpEntry {
        name: name.to_string(),
        kind,
        // A file holds its own path, which is also its size unless given.
        size: (kind == FILE).then(|| size.unwrap_or(format!("{folder}/{name}").len() as u64)),
        mtime_ms: None,
        mode: None,
        owner: None,
        group: None,
        download_ticket: None,
    }
}

/// A row of the listing of /srv, as selected in the file browser.
fn target(name: &'static str, kind: SftpEntryType) -> DownloadTarget {
    DownloadTarget {
        entry: entry("/srv", (name, kind, None)),
        path: format!("/srv/{name}"),
    }
}

type DownloadHook = Box<dyn Fn(&str, &mut DownloadResume) -> Option<ApiError>>;

/// Remote folders by path, the user's answer, and what the local folder refuses.
#[derive(Default)]
struct Remote {
    folders: Vec<(&'static str, Vec<RemoteEntry>)>,
    resolved: Vec<(&'static str, &'static str)>,
    decline: bool,
    /// Names the local folder turns down, as Windows turns down aux.c.
    refused: Vec<&'static str>,
    /// The local folder takes nothing.
    read_only: bool,
    calls: RefCell<Vec<String>>,
    asked: RefCell<Vec<DownloadQuestion>>,
    /// Runs before a file is downloaded; an error stops the download there.
    on_download: Option<DownloadHook>,
    on_confirm: Option<Box<dyn Fn()>>,
    on_create_folder: Option<Box<dyn Fn()>>,
}

impl Remote {
    fn new(folders: Vec<(&'static str, Vec<RemoteEntry>)>) -> Self {
        Self {
            folders,
            ..Self::default()
        }
    }

    fn calls(&self) -> Vec<String> {
        self.calls.borrow().clone()
    }

    fn downloads(&self) -> usize {
        self.calls()
            .iter()
            .filter(|call| call.starts_with("download "))
            .count()
    }

    fn refuses(&self, name: &str) -> io::Result<()> {
        if self.read_only {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "The request is not allowed.",
            ));
        }
        if self.refused.contains(&name) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidFilename,
                "Name is not allowed.",
            ));
        }
        Ok(())
    }
}

impl DownloadDeps for Remote {
    async fn list(&self, _conn_id: &str, path: &str) -> ApiResult<SftpListResponse> {
        self.calls.borrow_mut().push(format!("list {path}"));
        let Some((_, listing)) = self.folders.iter().find(|(folder, _)| *folder == path) else {
            return Err(ApiError::not_found("No such file"));
        };
        let resolved = self
            .resolved
            .iter()
            .find(|(from, _)| *from == path)
            .map_or(path, |(_, to)| to);
        Ok(SftpListResponse {
            path: resolved.to_string(),
            entries: listing.iter().map(|item| entry(path, *item)).collect(),
        })
    }

    async fn download(
        &self,
        _conn_id: &str,
        path: &str,
        resume: &mut DownloadResume,
        on_progress: &mut dyn FnMut(ByteProgress),
    ) -> ApiResult<Vec<u8>> {
        // A continued download asks for the rest and ends with the whole file.
        let call = match resume.range() {
            Some(range) => format!("download {path} from byte {}", range.start),
            None => format!("download {path}"),
        };
        self.calls.borrow_mut().push(call);
        if let Some(hook) = &self.on_download
            && let Some(error) = hook(path, resume)
        {
            return Err(error);
        }
        on_progress(ByteProgress {
            loaded: path.len() as u64,
            total: Some(path.len() as u64),
            bytes_per_second: 10.,
        });
        Ok(path.as_bytes().to_vec())
    }

    async fn confirm(&self, question: DownloadQuestion) -> bool {
        self.asked.borrow_mut().push(question);
        if let Some(hook) = &self.on_confirm {
            hook();
        }
        !self.decline
    }

    async fn create_folder(&self, parent: &Path, name: &str) -> io::Result<PathBuf> {
        if let Some(hook) = &self.on_create_folder {
            hook();
        }
        self.refuses(name)?;
        create_folder_in(parent, name)
    }

    async fn save(&self, folder: &Path, name: &str, bytes: Vec<u8>) -> io::Result<String> {
        self.refuses(name)?;
        save_into_folder(folder, name, &bytes)
    }
}

/// The local folder picked for a download, named Downloads.
struct Picked {
    _scratch: tempfile::TempDir,
    folder: PathBuf,
}

impl Picked {
    fn new() -> Self {
        let scratch = tempfile::tempdir().unwrap();
        let folder = scratch.path().join("Downloads");
        std::fs::create_dir(&folder).unwrap();
        Self {
            _scratch: scratch,
            folder,
        }
    }

    /// Every file below the folder with its text, and every folder, by `/` path.
    fn tree(&self) -> (Vec<(String, String)>, Vec<String>) {
        fn walk(
            root: &Path,
            dir: &Path,
            files: &mut Vec<(String, String)>,
            folders: &mut Vec<String>,
        ) {
            let mut children: Vec<_> = std::fs::read_dir(dir)
                .unwrap()
                .map(|child| child.unwrap().path())
                .collect();
            children.sort();
            for child in children {
                let relative = child
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                if child.is_dir() {
                    folders.push(relative);
                    walk(root, &child, files, folders);
                } else {
                    files.push((relative, std::fs::read_to_string(&child).unwrap()));
                }
            }
        }
        let (mut files, mut folders) = (Vec::new(), Vec::new());
        walk(&self.folder, &self.folder, &mut files, &mut folders);
        files.sort();
        folders.sort();
        (files, folders)
    }

    fn files(&self) -> Vec<(String, String)> {
        self.tree().0
    }

    fn file_names(&self) -> Vec<String> {
        self.files().into_iter().map(|(name, _)| name).collect()
    }

    fn folders(&self) -> Vec<String> {
        self.tree().1
    }
}

struct Run {
    result: ApiResult<Option<DownloadResult>>,
    progress: Vec<TransferProgressState>,
}

fn download(
    remote: &Remote,
    targets: &[DownloadTarget],
    picked: &Picked,
    stop: &TransferStop,
    store: &SharedInterruptedDownloads,
) -> Run {
    let mut progress = Vec::new();
    let result = block_on(download_from_remote(
        "conn-1",
        targets,
        &picked.folder,
        DownloadOptions {
            stop,
            interrupted: store,
            on_progress: &mut |update| progress.push(update.clone()),
            deps: remote,
        },
    ));
    Run { result, progress }
}

fn quietly(
    remote: &Remote,
    targets: &[DownloadTarget],
    picked: &Picked,
) -> ApiResult<Option<DownloadResult>> {
    download(remote, targets, picked, &TransferStop::new(), &store()).result
}

fn pairs(files: &[(&str, &str)]) -> Vec<(String, String)> {
    files
        .iter()
        .map(|(name, text)| (name.to_string(), text.to_string()))
        .collect()
}

#[test]
fn recreates_folders_below_the_picked_one_empty_ones_included_beside_the_selected_files() {
    let picked = Picked::new();
    let remote = Remote::new(vec![
        (
            "/srv/logs",
            vec![
                ("app.log", FILE, None),
                ("2024", DIR, None),
                ("empty", DIR, None),
            ],
        ),
        ("/srv/logs/2024", vec![("jan.log", FILE, None)]),
        ("/srv/logs/empty", vec![]),
    ]);

    let run = download(
        &remote,
        &[target("notes.txt", FILE), target("logs", DIR)],
        &picked,
        &TransferStop::new(),
        &store(),
    );

    assert_eq!(
        remote.calls(),
        [
            "list /srv/logs",
            "list /srv/logs/2024",
            "list /srv/logs/empty",
            "download /srv/notes.txt",
            "download /srv/logs/app.log",
            "download /srv/logs/2024/jan.log",
        ]
    );
    assert_eq!(picked.folders(), ["logs", "logs/2024", "logs/empty"]);
    assert_eq!(
        picked.files(),
        pairs(&[
            ("logs/2024/jan.log", "/srv/logs/2024/jan.log"),
            ("logs/app.log", "/srv/logs/app.log"),
            ("notes.txt", "/srv/notes.txt"),
        ])
    );
    assert_eq!(
        *remote.asked.borrow(),
        [DownloadQuestion {
            title: "Download 3 files (53 B)?".into(),
            description: "3 folders are created in Downloads.".into(),
            confirm_label: "Download".into(),
        }]
    );
    assert_eq!(
        run.result.unwrap(),
        Some(DownloadResult {
            downloaded: 3,
            file_count: 3,
            folders: 3,
            skipped: vec![],
            cancelled: false,
        })
    );
    assert!(
        run.progress
            .iter()
            .any(|update| update.phase == TransferPhase::Preparing && update.name == "logs/2024")
    );
    assert!(run.progress.iter().any(|update| {
        update.phase == TransferPhase::Transferring
            && update.name == "logs/2024/jan.log"
            && (update.loaded, update.total) == (53, Some(53))
            && (update.file_index, update.file_count) == (Some(3), Some(3))
    }));
    let last = run.progress.last().unwrap();
    assert_eq!(
        (last.phase, last.name.as_str(), last.loaded),
        (TransferPhase::Complete, "3 files", 53)
    );
    assert_eq!((last.file_index, last.file_count), (Some(3), Some(3)));
    assert!(
        run.progress
            .iter()
            .all(|update| update.direction == TransferDirection::Download)
    );
}

#[test]
fn numbers_a_folder_whose_name_is_taken_instead_of_merging_into_it() {
    let picked = Picked::new();
    std::fs::create_dir(picked.folder.join("logs")).unwrap();
    std::fs::write(picked.folder.join("logs/app.log"), "kept").unwrap();
    std::fs::write(picked.folder.join("logs (1)"), "a file").unwrap();
    let remote = Remote::new(vec![("/srv/logs", vec![("app.log", FILE, None)])]);

    quietly(&remote, &[target("logs", DIR)], &picked).unwrap();

    assert_eq!(picked.folders(), ["logs", "logs (2)"]);
    assert_eq!(
        picked.files(),
        pairs(&[
            ("logs (1)", "a file"),
            ("logs (2)/app.log", "/srv/logs/app.log"),
            ("logs/app.log", "kept"),
        ])
    );
}

#[test]
fn skips_links_and_special_files_without_following_them_and_names_them() {
    let picked = Picked::new();
    let remote = Remote::new(vec![(
        "/srv/logs",
        vec![
            ("app.log", FILE, None),
            ("current", SftpEntryType::Link, None),
            ("events", SftpEntryType::Other, None),
        ],
    )]);

    let result = quietly(&remote, &[target("logs", DIR)], &picked)
        .unwrap()
        .unwrap();

    assert_eq!(
        remote.calls(),
        ["list /srv/logs", "download /srv/logs/app.log"]
    );
    assert_eq!(
        result.skipped,
        ["logs/current (link)", "logs/events (special file)"]
    );
    assert_eq!(
        remote.asked.borrow()[0].description,
        "1 folder is created in Downloads. 2 items are skipped."
    );
    assert_eq!(picked.file_names(), ["logs/app.log"]);
}

#[test]
fn skips_names_that_are_not_a_single_entry_and_keeps_everything_in_the_picked_folder() {
    let picked = Picked::new();
    let remote = Remote::new(vec![(
        "/srv/logs",
        vec![
            ("ok.txt", FILE, None),
            ("nosuchdir/evil.sh", FILE, None),
            ("../outside.txt", FILE, None),
            ("x/y", DIR, None),
            ("..", DIR, None),
            ("", FILE, None),
            ("a\0b", FILE, None),
        ],
    )]);

    let result = quietly(
        &remote,
        &[target("logs", DIR), target("a/b", FILE)],
        &picked,
    )
    .unwrap()
    .unwrap();

    assert_eq!(
        remote.calls(),
        ["list /srv/logs", "download /srv/logs/ok.txt"]
    );
    assert_eq!(
        result.skipped,
        [
            "a/b (invalid name)",
            "logs/nosuchdir/evil.sh (invalid name)",
            "logs/../outside.txt (invalid name)",
            "logs/x/y (invalid name)",
            "logs/.. (invalid name)",
            "logs/ (invalid name)",
            "logs/a\0b (invalid name)",
        ]
    );
    assert_eq!(
        picked.files(),
        pairs(&[("logs/ok.txt", "/srv/logs/ok.txt")])
    );
    assert_eq!(picked.folders(), ["logs"]);
    // Nothing landed beside the picked folder either.
    assert_eq!(
        std::fs::read_dir(picked.folder.parent().unwrap())
            .unwrap()
            .count(),
        1
    );
}

#[test]
fn skips_names_the_local_folder_refuses_with_what_is_inside_such_a_folder_and_goes_on() {
    let picked = Picked::new();
    let remote = Remote {
        refused: vec!["aux.c", "con"],
        ..Remote::new(vec![
            (
                "/srv/linux",
                vec![
                    ("aux.c", FILE, None),
                    ("con", DIR, None),
                    ("main.c", FILE, None),
                ],
            ),
            ("/srv/linux/con", vec![("inside.c", FILE, None)]),
        ])
    };

    let result = quietly(&remote, &[target("linux", DIR)], &picked).unwrap();

    assert_eq!(
        remote.calls(),
        [
            "list /srv/linux",
            "list /srv/linux/con",
            "download /srv/linux/aux.c",
            "download /srv/linux/main.c"
        ]
    );
    assert_eq!(picked.folders(), ["linux"]);
    assert_eq!(
        picked.files(),
        pairs(&[("linux/main.c", "/srv/linux/main.c")])
    );
    assert_eq!(
        result,
        Some(DownloadResult {
            downloaded: 1,
            file_count: 2,
            folders: 1,
            skipped: vec![
                "linux/con (name not allowed locally)".into(),
                "linux/aux.c (name not allowed locally)".into()
            ],
            cancelled: false,
        })
    );
}

#[test]
fn skips_a_folder_that_leads_back_to_one_already_listed() {
    // A server that reports links with their target's attributes lists them as folders.
    let picked = Picked::new();
    let listing: Vec<RemoteEntry> = vec![("app.log", FILE, None), ("loop", DIR, None)];
    let remote = Remote {
        resolved: vec![("/srv/logs/loop", "/srv/logs")],
        ..Remote::new(vec![
            ("/srv/logs", listing.clone()),
            ("/srv/logs/loop", listing),
        ])
    };

    let result = quietly(&remote, &[target("logs", DIR)], &picked)
        .unwrap()
        .unwrap();

    assert_eq!(
        remote.calls(),
        [
            "list /srv/logs",
            "list /srv/logs/loop",
            "download /srv/logs/app.log"
        ]
    );
    assert_eq!((result.downloaded, result.folders), (1, 1));
    assert_eq!(result.skipped, ["logs/loop (link)"]);
    assert_eq!(picked.folders(), ["logs"]);
}

#[test]
fn refuses_folders_past_the_limits_before_saving_anything() {
    let picked = Picked::new();
    let names: Vec<&'static str> = (0..MAX_FOLDER_DOWNLOAD_ENTRIES)
        .map(|index| &*Box::leak(format!("{index}.log").into_boxed_str()))
        .collect();
    let crowded = Remote::new(vec![(
        "/srv/logs",
        names.iter().map(|name| (*name, FILE, None)).collect(),
    )]);
    let large = Remote::new(vec![(
        "/srv/images",
        vec![
            ("a.bin", FILE, Some(MAX_FOLDER_DOWNLOAD_BYTES)),
            ("b.bin", FILE, Some(1)),
        ],
    )]);

    let too_many = quietly(&crowded, &[target("logs", DIR)], &picked).unwrap_err();
    assert_eq!(
        too_many.message,
        "More than 10,000 files and folders to download. Download fewer or smaller folders."
    );
    let too_much = quietly(&large, &[target("images", DIR)], &picked).unwrap_err();
    assert_eq!(
        too_much.message,
        "More than 10 GiB to download. Download fewer or smaller folders."
    );
    for remote in [&crowded, &large] {
        assert!(remote.asked.borrow().is_empty());
        assert_eq!(remote.downloads(), 0);
    }
    assert!(picked.folders().is_empty());
}

#[test]
fn stops_on_a_folder_that_cannot_be_listed_naming_it_before_saving_anything() {
    let picked = Picked::new();
    let remote = Remote::new(vec![("/srv/logs", vec![("private", DIR, None)])]);

    let error = quietly(&remote, &[target("logs", DIR)], &picked).unwrap_err();

    assert_eq!(
        (error.status, error.message.as_str()),
        (404, "Could not list logs/private: No such file")
    );
    assert!(picked.folders().is_empty());
}

#[test]
fn saves_nothing_when_the_question_is_declined() {
    let picked = Picked::new();
    let remote = Remote {
        decline: true,
        ..Remote::new(vec![("/srv/logs", vec![("app.log", FILE, None)])])
    };

    assert_eq!(quietly(&remote, &[target("logs", DIR)], &picked), Ok(None));
    assert_eq!(remote.downloads(), 0);
    assert!(picked.folders().is_empty());
    assert!(picked.files().is_empty());
}

#[test]
fn downloads_files_alone_without_asking() {
    let picked = Picked::new();
    let remote = Remote::new(vec![]);

    quietly(
        &remote,
        &[target("a.txt", FILE), target("b.txt", FILE)],
        &picked,
    )
    .unwrap();

    assert!(remote.asked.borrow().is_empty());
    assert_eq!(picked.file_names(), ["a.txt", "b.txt"]);
}

// --- stopping and continuing a folder download ------------------------------------------------

fn logs() -> Vec<(&'static str, Vec<RemoteEntry>)> {
    vec![(
        "/srv/logs",
        vec![
            ("a.log", FILE, None),
            ("b.log", FILE, None),
            ("c.log", FILE, None),
        ],
    )]
}

/// Downloading b.log stops after its first bytes, with the error `stop` returns.
fn stop_at_second_file(stop: impl Fn() -> ApiError + 'static) -> Option<DownloadHook> {
    Some(Box::new(move |path, resume| {
        if !path.contains("b.log") {
            return None;
        }
        let error = stop();
        resume.interrupted(partial("/srv/"));
        Some(error)
    }))
}

#[test]
fn continues_a_file_from_what_an_interrupted_download_received_and_forgets_it_once_saved() {
    let store = store();
    keep(&store, "/srv/logs/b.log", "/srv/logs/");
    let picked = Picked::new();
    let remote = Remote::new(logs());

    download(
        &remote,
        &[target("logs", DIR)],
        &picked,
        &TransferStop::new(),
        &store,
    )
    .result
    .unwrap();

    assert_eq!(
        remote.calls(),
        [
            "list /srv/logs",
            "download /srv/logs/a.log",
            "download /srv/logs/b.log from byte 10",
            "download /srv/logs/c.log",
        ]
    );
    assert_eq!(
        std::fs::read_to_string(picked.folder.join("logs/b.log")).unwrap(),
        "/srv/logs/b.log"
    );
    assert_eq!(kept(&store, "/srv/logs/b.log"), None);
}

#[test]
fn keeps_what_was_saved_after_cancel_says_how_far_it_got_and_drops_what_the_file_received() {
    let store = store();
    keep(&store, "/srv/logs/b.log", "/srv/logs/");
    let stop = TransferStop::new();
    let picked = Picked::new();
    let remote = Remote {
        on_download: stop_at_second_file({
            let stop = stop.clone();
            move || {
                stop.stop(StopReason::Cancelled);
                super::super::transfer::abort_error(StopReason::Cancelled)
            }
        }),
        ..Remote::new(logs())
    };

    let result = download(&remote, &[target("logs", DIR)], &picked, &stop, &store).result;

    assert_eq!(
        result,
        Ok(Some(DownloadResult {
            downloaded: 1,
            file_count: 3,
            folders: 1,
            skipped: vec![],
            cancelled: true,
        }))
    );
    assert_eq!(picked.file_names(), ["logs/a.log"]);
    assert_eq!(remote.downloads(), 2);
    assert_eq!(kept(&store, "/srv/logs/b.log"), None);
}

#[test]
fn fails_with_the_stop_when_the_browser_closes_or_the_connection_is_lost_keeping_what_the_file_received()
 {
    for reason in [StopReason::Closed, StopReason::ConnectionLost] {
        let store = store();
        let stop = TransferStop::new();
        let picked = Picked::new();
        let remote = Remote {
            on_download: stop_at_second_file({
                let stop = stop.clone();
                move || {
                    stop.stop(reason);
                    super::super::transfer::abort_error(reason)
                }
            }),
            ..Remote::new(logs())
        };

        let error = download(&remote, &[target("logs", DIR)], &picked, &stop, &store)
            .result
            .unwrap_err();

        // The stop itself, not a failure naming the file.
        assert_eq!(error, super::super::transfer::abort_error(reason));
        assert_eq!(picked.file_names(), ["logs/a.log"]);
        assert_eq!(kept(&store, "/srv/logs/b.log").as_deref(), Some("/srv/"));
    }
}

#[test]
fn fails_with_a_failure_naming_the_file_and_keeping_what_it_received() {
    let store = store();
    let picked = Picked::new();
    let remote = Remote {
        on_download: stop_at_second_file(|| ApiError::internal("network error")),
        ..Remote::new(logs())
    };

    let error = download(
        &remote,
        &[target("logs", DIR)],
        &picked,
        &TransferStop::new(),
        &store,
    )
    .result
    .unwrap_err();

    assert_eq!(
        error.message,
        "Could not download logs/b.log: network error"
    );
    assert_eq!(picked.file_names(), ["logs/a.log"]);
    assert_eq!(kept(&store, "/srv/logs/b.log").as_deref(), Some("/srv/"));
}

#[test]
fn names_a_file_or_folder_the_picked_folder_does_not_take() {
    let picked = Picked::new();
    let remote = Remote {
        read_only: true,
        ..Remote::new(vec![("/srv/logs", vec![])])
    };

    let files = quietly(
        &remote,
        &[target("a.txt", FILE), target("b.txt", FILE)],
        &picked,
    )
    .unwrap_err();
    assert_eq!(
        files.message,
        "Could not save a.txt: The request is not allowed."
    );
    assert_eq!(remote.downloads(), 1);

    let folders = quietly(&remote, &[target("logs", DIR)], &picked).unwrap_err();
    assert_eq!(
        folders.message,
        "Could not create logs: The request is not allowed."
    );
}

#[test]
fn stops_creating_folders_after_cancel_and_says_how_many_it_created() {
    let stop = TransferStop::new();
    let picked = Picked::new();
    let remote = Remote {
        on_create_folder: Some(Box::new({
            let stop = stop.clone();
            move || stop.stop(StopReason::Cancelled)
        })),
        ..Remote::new(vec![
            ("/srv/a", vec![]),
            ("/srv/b", vec![]),
            ("/srv/c", vec![("c.log", FILE, None)]),
        ])
    };

    let result = download(
        &remote,
        &[target("a", DIR), target("b", DIR), target("c", DIR)],
        &picked,
        &stop,
        &store(),
    )
    .result
    .unwrap()
    .unwrap();

    assert_eq!(picked.folders(), ["a"]);
    assert_eq!(remote.downloads(), 0);
    assert_eq!(
        result,
        DownloadResult {
            downloaded: 0,
            file_count: 0,
            folders: 1,
            skipped: vec![],
            cancelled: true,
        }
    );
    assert_eq!(
        download_summary(&result, folder_name(&picked.folder).as_deref()),
        "Download cancelled: 1 folder created in Downloads"
    );
}

#[test]
fn leaves_the_files_it_finished_alone_when_cancelled_later() {
    let store = store();
    let stop = TransferStop::new();
    let picked = Picked::new();
    let remote = Remote {
        on_download: Some(Box::new({
            let (stop, store) = (stop.clone(), store.clone());
            move |path, _| {
                if !path.contains("c.log") {
                    return None;
                }
                // Meanwhile another file browser in the window was cut off while downloading
                // a.log.
                keep(&store, "/srv/logs/a.log", "/srv/");
                stop.stop(StopReason::Cancelled);
                Some(super::super::transfer::abort_error(StopReason::Cancelled))
            }
        })),
        ..Remote::new(logs())
    };

    let result = download(&remote, &[target("logs", DIR)], &picked, &stop, &store)
        .result
        .unwrap()
        .unwrap();

    assert_eq!((result.downloaded, result.cancelled), (2, true));
    assert_eq!(kept(&store, "/srv/logs/a.log").as_deref(), Some("/srv/"));
}

#[test]
fn stops_before_saving_anything_when_the_connection_is_lost_while_asking() {
    let stop = TransferStop::new();
    let picked = Picked::new();
    let remote = Remote {
        on_confirm: Some(Box::new({
            let stop = stop.clone();
            move || stop.stop(StopReason::ConnectionLost)
        })),
        ..Remote::new(logs())
    };

    let error = download(&remote, &[target("logs", DIR)], &picked, &stop, &store())
        .result
        .unwrap_err();

    assert_eq!(
        error,
        super::super::transfer::abort_error(StopReason::ConnectionLost)
    );
    assert!(picked.folders().is_empty());
    assert_eq!(remote.downloads(), 0);
}

// --- download summaries -----------------------------------------------------------------------

#[test]
fn says_what_was_saved_where_and_what_was_left_out() {
    let nothing = DownloadResult::default();
    assert_eq!(
        download_summary(
            &DownloadResult {
                downloaded: 3,
                file_count: 3,
                ..nothing.clone()
            },
            Some("Downloads")
        ),
        "Downloaded 3 files to Downloads"
    );
    assert_eq!(
        download_summary(
            &DownloadResult {
                downloaded: 1,
                file_count: 1,
                folders: 1,
                skipped: vec!["logs/current (link)".into()],
                ..nothing.clone()
            },
            None
        ),
        "Downloaded 1 file; 1 item skipped"
    );
    assert_eq!(
        download_summary(
            &DownloadResult {
                folders: 2,
                ..nothing.clone()
            },
            Some("Downloads")
        ),
        "Created 2 folders in Downloads"
    );
    assert_eq!(download_summary(&nothing, None), "Nothing was downloaded");
}

#[test]
fn says_how_far_a_cancelled_download_got() {
    let cancelled = DownloadResult {
        cancelled: true,
        ..DownloadResult::default()
    };
    assert_eq!(
        download_summary(
            &DownloadResult {
                downloaded: 12,
                file_count: 47,
                folders: 6,
                ..cancelled.clone()
            },
            Some("Downloads")
        ),
        "Download cancelled: 12 of 47 files saved in Downloads"
    );
    assert_eq!(
        download_summary(
            &DownloadResult {
                file_count: 47,
                folders: 3,
                ..cancelled.clone()
            },
            Some("Downloads")
        ),
        "Download cancelled: 3 folders created in Downloads"
    );
    assert_eq!(
        download_summary(
            &DownloadResult {
                file_count: 47,
                ..cancelled
            },
            None
        ),
        "Download cancelled"
    );
}

#[test]
fn names_the_picked_folder_for_messages() {
    assert_eq!(
        folder_name(Path::new("/home/me/Downloads")).as_deref(),
        Some("Downloads")
    );
    assert_eq!(folder_name(Path::new("/")), None);
}
