//! `tests/unit/electron/link-handlers.test.ts`. xdg-mime and the system APIs are fakes over
//! scratch folders; nothing here changes a real default.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use muxus_core::api::{LinkHandlerState, LinkHandlerStatus};
use muxus_core::connection_links::ConnectionLinkScheme;
use muxus_core::platform::Platform;

use super::*;

struct Scratch {
    _dir: tempfile::TempDir,
    root: PathBuf,
    data_home: PathBuf,
    system_data: PathBuf,
}

fn scratch() -> Scratch {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_path_buf();
    let system_data = root.join("system");
    std::fs::create_dir_all(system_data.join("applications")).unwrap();
    Scratch {
        data_home: root.join("data"),
        system_data,
        root,
        _dir: dir,
    }
}

/// xdg-mime stand-in over an in-memory mimeapps.list.
#[derive(Default)]
struct FakeXdgMime {
    defaults: Mutex<HashMap<String, String>>,
    calls: Mutex<Vec<Vec<String>>>,
}

#[async_trait]
impl CommandRunner for FakeXdgMime {
    async fn run(
        &self,
        command: &str,
        args: &[String],
        _env: &HashMap<String, String>,
    ) -> Result<String, RunError> {
        let mut call = vec![command.to_string()];
        call.extend(args.iter().cloned());
        self.calls.lock().unwrap().push(call);
        if command != "xdg-mime" {
            return Err(RunError::NotFound);
        }
        match args
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice()
        {
            ["query", "default", mime] => Ok(format!(
                "{}\n",
                self.defaults
                    .lock()
                    .unwrap()
                    .get(*mime)
                    .cloned()
                    .unwrap_or_default()
            )),
            ["default", id, mime] => {
                self.defaults
                    .lock()
                    .unwrap()
                    .insert(mime.to_string(), id.to_string());
                Ok(String::new())
            }
            other => Err(RunError::Failed(format!("unexpected xdg-mime {other:?}"))),
        }
    }
}

fn handlers(
    scratch: &Scratch,
    run: Arc<dyn CommandRunner>,
    command: Option<Vec<String>>,
    installed: bool,
    alias_directory: Option<PathBuf>,
) -> LinuxLinkHandlers {
    let env: HashMap<String, String> = [
        (
            "XDG_DATA_HOME".to_string(),
            scratch.data_home.to_string_lossy().into_owned(),
        ),
        (
            "XDG_DATA_DIRS".to_string(),
            scratch.system_data.to_string_lossy().into_owned(),
        ),
    ]
    .into();
    LinuxLinkHandlers::new(LinuxLinkHandlerOptions {
        env,
        home: scratch.root.clone(),
        command: command.unwrap_or_else(|| vec!["/home/me/Apps/Muxus 0.10.0.AppImage".into()]),
        installed,
        alias_directory,
        run,
    })
}

fn exec_arguments(entry: &str) -> Option<Vec<String>> {
    let exec = parse_key_file_group(entry, "Desktop Entry")
        .get("Exec")
        .cloned()
        .unwrap_or_default();
    split_exec(&unescape_desktop_string(&exec))
}

fn generated(scratch: &Scratch) -> String {
    std::fs::read_to_string(
        scratch
            .data_home
            .join("applications")
            .join(GENERATED_DESKTOP_ID),
    )
    .unwrap()
}

fn status(is_default: bool, current_handler: Option<&str>) -> LinkHandlerStatus {
    LinkHandlerStatus {
        is_default,
        current_handler: current_handler.map(str::to_string),
    }
}

// --- generated desktop entry ---

#[test]
fn quotes_every_launch_argument_so_it_reads_back_unchanged() {
    let command = vec![
        "/home/me/Apps/Muxus 0.10.0.AppImage".to_string(),
        "/opt/odd \"quoted\" $HOME `tick` back\\slash 100%".into(),
        "--no-sandbox".into(),
    ];
    let entry = generated_desktop_entry(&command, &[ConnectionLinkScheme::Ssh]).unwrap();
    assert_eq!(
        exec_arguments(&entry).unwrap(),
        [
            command[0].as_str(),
            "/opt/odd \"quoted\" $HOME `tick` back\\slash 100%%",
            "--no-sandbox",
            "%u"
        ]
    );
}

#[test]
fn claims_only_the_chosen_schemes_and_stays_out_of_menus() {
    let values = parse_key_file_group(
        &generated_desktop_entry(&["/usr/bin/muxus".into()], &[ConnectionLinkScheme::Ssh]).unwrap(),
        "Desktop Entry",
    );
    assert_eq!(values["MimeType"], "x-scheme-handler/ssh;");
    assert_eq!(values["NoDisplay"], "true");
    assert_eq!(values["Exec"], "/usr/bin/muxus %u");
    let both = parse_key_file_group(
        &generated_desktop_entry(
            &["/usr/bin/muxus".into()],
            &[ConnectionLinkScheme::Ssh, ConnectionLinkScheme::Telnet],
        )
        .unwrap(),
        "Desktop Entry",
    );
    assert_eq!(
        both["MimeType"],
        "x-scheme-handler/ssh;x-scheme-handler/telnet;"
    );
}

#[test]
fn refuses_paths_that_would_break_the_file_apart() {
    assert!(desktop_exec_argument("/tmp/a\nExec=evil").is_err());
    assert_eq!(
        desktop_exec_argument("/opt/Muxus/muxus").unwrap(),
        "/opt/Muxus/muxus"
    );
}

// --- AppImage environment ---

#[test]
fn keeps_the_appimage_mount_away_from_xdg_mime() {
    let mount = "/tmp/.mount_Muxus 1BHZoT";
    let env: HashMap<String, String> = [
        ("APPIMAGE", "/home/me/Muxus Test.AppImage".to_string()),
        ("APPDIR", mount.to_string()),
        ("PATH", format!("{mount}:{mount}/usr/sbin:/usr/bin:/bin")),
        (
            "XDG_DATA_DIRS",
            format!("{mount}/usr/share/:/usr/share/gnome:/usr/local/share/:/usr/share/"),
        ),
        ("LD_LIBRARY_PATH", format!("{mount}/usr/lib")),
        ("HOME", "/home/me".to_string()),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value))
    .collect();
    let expected: HashMap<String, String> = [
        ("APPIMAGE", "/home/me/Muxus Test.AppImage"),
        ("APPDIR", mount),
        ("PATH", "/usr/bin:/bin"),
        (
            "XDG_DATA_DIRS",
            "/usr/share/gnome:/usr/local/share/:/usr/share/",
        ),
        ("HOME", "/home/me"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    assert_eq!(without_app_image_paths(&env), expected);
}

#[test]
fn leaves_other_builds_alone() {
    let env: HashMap<String, String> = [
        ("PATH", "/usr/bin"),
        ("XDG_DATA_DIRS", "/usr/share"),
        ("APPDIR", "/opt/app"),
    ]
    .into_iter()
    .map(|(key, value)| (key.to_string(), value.to_string()))
    .collect();
    assert_eq!(without_app_image_paths(&env), env);
}

// --- Linux link handler registration ---

#[tokio::test]
async fn reports_the_current_handlers_by_name_without_changing_anything() {
    let scratch = scratch();
    std::fs::write(
        scratch
            .system_data
            .join("applications")
            .join("putty.desktop"),
        "[Desktop Entry]\nType=Application\nName=PuTTY SSH Client\nExec=putty %u\n",
    )
    .unwrap();
    let xdg = Arc::new(FakeXdgMime::default());
    xdg.defaults
        .lock()
        .unwrap()
        .insert("x-scheme-handler/ssh".into(), "putty.desktop".into());
    let state = handlers(&scratch, xdg.clone(), None, false, None)
        .state()
        .await;
    assert_eq!(
        state,
        LinkHandlerState {
            unavailable: None,
            ssh: status(false, Some("PuTTY SSH Client")),
            telnet: status(false, None),
        }
    );
    assert!(
        xdg.calls
            .lock()
            .unwrap()
            .iter()
            .all(|call| call[1] == "query")
    );
}

#[tokio::test]
async fn points_an_appimage_registration_at_the_appimage_file() {
    let scratch = scratch();
    let xdg = Arc::new(FakeXdgMime::default());
    let linux = handlers(&scratch, xdg.clone(), None, false, None);
    let result = linux.register(ConnectionLinkScheme::Ssh).await;
    assert_eq!(result.error, None);
    assert_eq!(
        xdg.defaults
            .lock()
            .unwrap()
            .get("x-scheme-handler/ssh")
            .map(String::as_str),
        Some(GENERATED_DESKTOP_ID)
    );
    assert!(
        !xdg.defaults
            .lock()
            .unwrap()
            .contains_key("x-scheme-handler/telnet")
    );
    let entry = generated(&scratch);
    assert_eq!(
        exec_arguments(&entry).unwrap(),
        ["/home/me/Apps/Muxus 0.10.0.AppImage", "%u"]
    );
    assert_eq!(
        parse_key_file_group(&entry, "Desktop Entry")["MimeType"],
        "x-scheme-handler/ssh;"
    );
    assert_eq!(result.state.ssh, status(true, Some("Muxus")));
    assert!(!result.state.telnet.is_default);

    // Adding telnet keeps ssh claimed.
    let both = linux.register(ConnectionLinkScheme::Telnet).await;
    assert!(both.state.ssh.is_default && both.state.telnet.is_default);
    assert_eq!(
        parse_key_file_group(&generated(&scratch), "Desktop Entry")["MimeType"],
        "x-scheme-handler/ssh;x-scheme-handler/telnet;"
    );
}

#[tokio::test]
async fn notices_a_moved_appimage_and_registers_the_new_location() {
    let scratch = scratch();
    let xdg = Arc::new(FakeXdgMime::default());
    handlers(&scratch, xdg.clone(), None, false, None)
        .register(ConnectionLinkScheme::Ssh)
        .await;
    let moved = handlers(
        &scratch,
        xdg.clone(),
        Some(vec!["/opt/apps/Muxus.AppImage".into()]),
        false,
        None,
    );
    assert_eq!(
        moved.state().await.ssh,
        status(false, Some("Another copy of Muxus"))
    );
    assert!(
        moved
            .register(ConnectionLinkScheme::Ssh)
            .await
            .state
            .ssh
            .is_default
    );
}

#[cfg(unix)]
#[tokio::test]
async fn reaches_an_appimage_whose_path_needs_quoting_through_a_plain_link() {
    let scratch = scratch();
    let apps = scratch.root.join("My Apps");
    std::fs::create_dir(&apps).unwrap();
    let app_image = apps.join("Muxus 0.10.0.AppImage");
    std::fs::write(&app_image, "").unwrap();
    let alias_directory = scratch.root.join("userdata");
    let xdg = Arc::new(FakeXdgMime::default());
    let linux = handlers(
        &scratch,
        xdg,
        Some(vec![app_image.to_string_lossy().into_owned()]),
        false,
        Some(alias_directory.clone()),
    );
    let result = linux.register(ConnectionLinkScheme::Ssh).await;
    let alias = alias_directory.join("muxus-link-handler");
    // xdg-utils reads the first space-separated word of Exec as the program.
    assert_eq!(
        parse_key_file_group(&generated(&scratch), "Desktop Entry")["Exec"],
        format!("{} %u", alias.display())
    );
    assert_eq!(std::fs::read_link(&alias).unwrap(), app_image);
    assert!(result.state.ssh.is_default);

    // Moving the AppImage leaves the link dangling, so the choice is offered again.
    std::fs::rename(&app_image, apps.join("Muxus 0.10.0.AppImage.moved")).unwrap();
    assert!(!linux.state().await.ssh.is_default);
}

#[tokio::test]
async fn uses_the_desktop_file_a_package_installed() {
    let scratch = scratch();
    std::fs::write(
        scratch.system_data.join("applications").join(INSTALLED_DESKTOP_ID),
        "[Desktop Entry]\nType=Application\nName=Muxus\nExec=/opt/Muxus/muxus %U\nMimeType=x-scheme-handler/ssh;x-scheme-handler/telnet;\n",
    )
    .unwrap();
    let xdg = Arc::new(FakeXdgMime::default());
    let linux = handlers(
        &scratch,
        xdg.clone(),
        Some(vec!["/opt/Muxus/muxus".into()]),
        true,
        None,
    );
    let result = linux.register(ConnectionLinkScheme::Telnet).await;
    assert!(xdg.calls.lock().unwrap().contains(&vec![
        "xdg-mime".to_string(),
        "default".into(),
        INSTALLED_DESKTOP_ID.into(),
        "x-scheme-handler/telnet".into()
    ]));
    assert_eq!(result.state.telnet, status(true, Some("Muxus")));
    assert!(
        !scratch
            .data_home
            .join("applications")
            .join(GENERATED_DESKTOP_ID)
            .exists()
    );
}

#[tokio::test]
async fn falls_back_to_its_own_entry_when_the_package_entry_is_missing() {
    let scratch = scratch();
    let xdg = Arc::new(FakeXdgMime::default());
    handlers(
        &scratch,
        xdg.clone(),
        Some(vec!["/opt/Muxus/muxus".into()]),
        true,
        None,
    )
    .register(ConnectionLinkScheme::Ssh)
    .await;
    assert_eq!(
        xdg.defaults
            .lock()
            .unwrap()
            .get("x-scheme-handler/ssh")
            .map(String::as_str),
        Some(GENERATED_DESKTOP_ID)
    );
}

struct Missing;

#[async_trait]
impl CommandRunner for Missing {
    async fn run(
        &self,
        _command: &str,
        _args: &[String],
        _env: &HashMap<String, String>,
    ) -> Result<String, RunError> {
        Err(RunError::NotFound)
    }
}

#[tokio::test]
async fn explains_a_missing_xdg_mime() {
    let scratch = scratch();
    let state = handlers(&scratch, Arc::new(Missing), None, false, None)
        .state()
        .await;
    assert!(state.unavailable.unwrap().contains("xdg-utils"));
    let result = handlers(&scratch, Arc::new(Missing), None, false, None)
        .register(ConnectionLinkScheme::Ssh)
        .await;
    assert!(result.error.unwrap().contains("xdg-utils"));
}

struct Stubborn;

#[async_trait]
impl CommandRunner for Stubborn {
    async fn run(
        &self,
        _command: &str,
        args: &[String],
        _env: &HashMap<String, String>,
    ) -> Result<String, RunError> {
        Ok(if args[0] == "query" {
            "putty.desktop\n".into()
        } else {
            String::new()
        })
    }
}

#[tokio::test]
async fn reports_a_desktop_that_ignores_the_choice() {
    let scratch = scratch();
    let result = handlers(&scratch, Arc::new(Stubborn), None, false, None)
        .register(ConnectionLinkScheme::Ssh)
        .await;
    assert!(result.error.unwrap().contains("did not accept"));
    assert!(!result.state.ssh.is_default);
}

// --- macOS and Windows link handler registration ---

struct FakeApi {
    registered: Mutex<Vec<String>>,
    default_schemes: Mutex<Vec<String>>,
    names: HashMap<String, String>,
    handler_path: Mutex<Option<String>>,
    /// The path Windows resolves after a registration.
    path_after_register: Option<String>,
    opened: Mutex<usize>,
}

impl FakeApi {
    fn new() -> Self {
        Self {
            registered: Mutex::default(),
            default_schemes: Mutex::default(),
            names: HashMap::new(),
            handler_path: Mutex::default(),
            path_after_register: None,
            opened: Mutex::new(0),
        }
    }
}

#[async_trait]
impl ProtocolClientApi for FakeApi {
    async fn is_default_protocol_client(&self, scheme: &str) -> bool {
        self.default_schemes
            .lock()
            .unwrap()
            .iter()
            .any(|s| s == scheme)
    }

    async fn set_as_default_protocol_client(&self, scheme: &str) -> bool {
        self.registered.lock().unwrap().push(scheme.to_string());
        if let Some(path) = &self.path_after_register {
            *self.handler_path.lock().unwrap() = Some(path.clone());
        }
        true
    }

    async fn application_name_for_protocol(&self, url: &str) -> String {
        self.names.get(url).cloned().unwrap_or_default()
    }

    async fn application_path_for_protocol(&self, _url: &str) -> Option<String> {
        self.handler_path.lock().unwrap().clone()
    }

    async fn open_default_apps_settings(&self) -> Result<(), String> {
        *self.opened.lock().unwrap() += 1;
        Ok(())
    }
}

const EXECUTABLE: &str = "C:\\Users\\me\\AppData\\Local\\Programs\\Muxus\\Muxus.exe";

fn system(platform: Platform, api: Arc<FakeApi>, windows_store: bool) -> SystemLinkHandlers {
    SystemLinkHandlers::new(SystemLinkHandlerOptions {
        platform,
        api,
        windows_store,
        executable: EXECUTABLE.into(),
    })
}

#[tokio::test]
async fn reads_launch_services_on_macos() {
    let mut api = FakeApi::new();
    api.names = [
        ("ssh://".to_string(), "Muxus".to_string()),
        ("telnet://".to_string(), "Terminal".to_string()),
    ]
    .into();
    api.default_schemes.lock().unwrap().push("ssh".into());
    let mac = system(Platform::MacOs, Arc::new(api), false);
    assert_eq!(
        mac.state().await,
        LinkHandlerState {
            unavailable: None,
            ssh: status(true, Some("Muxus")),
            telnet: status(false, Some("Terminal")),
        }
    );
}

#[tokio::test]
async fn registers_the_running_executable_on_windows_and_checks_what_windows_resolves() {
    let mut api = FakeApi::new();
    *api.handler_path.lock().unwrap() = Some("C:\\Program Files\\PuTTY\\putty.exe".into());
    api.path_after_register = Some(EXECUTABLE.to_uppercase());
    let api = Arc::new(api);
    let windows = system(Platform::Windows, api.clone(), false);
    assert!(!windows.state().await.ssh.is_default);
    let result = windows.register(ConnectionLinkScheme::Ssh).await;
    assert_eq!(*api.registered.lock().unwrap(), ["ssh"]);
    assert!(result.state.ssh.is_default);
    assert_eq!(result.opened_system_settings, None);
    assert_eq!(*api.opened.lock().unwrap(), 0);
}

#[tokio::test]
async fn opens_default_apps_when_the_users_own_choice_outranks_the_registration() {
    let api = FakeApi::new();
    *api.handler_path.lock().unwrap() = Some("C:\\PuTTY\\putty.exe".into());
    let api = Arc::new(api);
    let windows = system(Platform::Windows, api.clone(), false);
    let result = windows.register(ConnectionLinkScheme::Telnet).await;
    assert_eq!(result.opened_system_settings, Some(true));
    assert_eq!(*api.opened.lock().unwrap(), 1);
}

#[tokio::test]
async fn leaves_the_registry_alone_for_a_store_package() {
    let api = Arc::new(FakeApi::new());
    let store = system(Platform::Windows, api.clone(), true);
    let result = store.register(ConnectionLinkScheme::Ssh).await;
    assert_eq!(result.opened_system_settings, Some(true));
    assert!(api.registered.lock().unwrap().is_empty());
    assert_eq!(*api.opened.lock().unwrap(), 1);
}

#[tokio::test]
async fn reports_a_refused_registration_on_macos() {
    let api = Arc::new(FakeApi::new());
    let mac = system(Platform::MacOs, api.clone(), false);
    let result = mac.register(ConnectionLinkScheme::Ssh).await;
    assert!(result.error.unwrap().contains("did not accept"));
    assert_eq!(*api.registered.lock().unwrap(), ["ssh"]);
}

// --- Desktop Entry helpers ---

#[test]
fn reads_desktop_entries_like_the_desktop_does() {
    let text = "# comment\n[Desktop Entry]\nName=First\nName=Second\nExec=a\n[Desktop Entry]\nName=Ignored\n[Other]\nName=Other\n";
    let values = parse_key_file_group(text, "Desktop Entry");
    assert_eq!(values["Name"], "First");
    assert_eq!(parse_key_file_group(text, "Other")["Name"], "Other");
    assert_eq!(unescape_desktop_string(r"a\sb\\c\nd\x"), "a b\\c\nd\\x");
    assert_eq!(
        split_exec(r#"/usr/bin/app "two words" plain\ escaped %u"#).unwrap(),
        ["/usr/bin/app", "two words", "plain escaped", "%u"]
    );
    assert_eq!(split_exec("\"unterminated"), None);
    assert_eq!(split_exec("   "), None);
}

#[test]
fn lists_data_directories_most_important_first() {
    let env: HashMap<String, String> = [
        ("XDG_DATA_HOME".to_string(), "/scratch/data".to_string()),
        (
            "XDG_DATA_DIRS".to_string(),
            "/a:relative:/scratch/data:/b".to_string(),
        ),
    ]
    .into();
    assert_eq!(
        data_directories(&env, Path::new("/home/me")),
        [
            PathBuf::from("/scratch/data"),
            PathBuf::from("/a"),
            PathBuf::from("/b")
        ]
    );
    assert_eq!(
        data_directories(&HashMap::new(), Path::new("/home/me")),
        [
            PathBuf::from("/home/me/.local/share"),
            PathBuf::from("/usr/local/share"),
            PathBuf::from("/usr/share")
        ]
    );
}

#[test]
fn reads_windows_registry_answers() {
    let output = "\r\nHKEY_CLASSES_ROOT\\ssh\\shell\\open\\command\r\n    (Default)    REG_SZ    \"C:\\Program Files\\PuTTY\\putty.exe\" \"%1\"\r\n";
    let command = WindowsProtocolClient::default_value(output).unwrap();
    assert_eq!(
        WindowsProtocolClient::command_program(&command).unwrap(),
        "C:\\Program Files\\PuTTY\\putty.exe"
    );
    let choice = "    ProgId    REG_SZ    Muxus.ssh\r\n";
    assert_eq!(
        WindowsProtocolClient::named_value(choice, "ProgId").unwrap(),
        "Muxus.ssh"
    );
    assert!(same_windows_path(
        "c:/users/ME/muxus.exe",
        "C:\\Users\\me\\Muxus.exe"
    ));
}

// --- The backend routes ---

#[cfg(target_os = "linux")]
#[test]
fn the_backend_reads_and_registers_link_handlers_in_its_own_home() {
    use std::os::unix::fs::PermissionsExt;
    let scratch = scratch();
    let bin = scratch.root.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let defaults = scratch.root.join("defaults");
    // An xdg-mime that keeps its defaults in a file.
    std::fs::write(
        bin.join("xdg-mime"),
        format!(
            "#!/bin/sh\nDB='{}'\ntouch \"$DB\"\nif [ \"$1\" = query ]; then grep \"^$3=\" \"$DB\" | tail -n 1 | cut -d= -f2; exit 0; fi\nif [ \"$1\" = default ]; then echo \"$3=$2\" >> \"$DB\"; exit 0; fi\nexit 1\n",
            defaults.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(bin.join("xdg-mime"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let home = scratch.root.join("home");
    let data = scratch.root.join("muxus-data");
    let mut options = crate::BackendOptions::sandbox(&data, &home);
    if let Some(environment) = options.local_environment.as_mut() {
        environment
            .vars
            .insert("PATH".into(), format!("{}:/usr/bin:/bin", bin.display()));
        environment.vars.remove("APPIMAGE");
    }
    let backend = crate::Backend::start_blocking(options).unwrap();
    let state = run(backend.link_handlers()).unwrap().unwrap();
    assert_eq!(state.unavailable, None);
    assert!(!state.ssh.is_default && !state.telnet.is_default);
    let result = run(backend.register_link_handler(ConnectionLinkScheme::Telnet))
        .unwrap()
        .unwrap();
    assert_eq!(result.error, None);
    assert!(result.state.telnet.is_default);
    assert!(!result.state.ssh.is_default);
    // Everything it wrote stayed in the sandbox.
    let entry = home
        .join(".local/share/applications")
        .join(GENERATED_DESKTOP_ID);
    assert!(entry.exists());
    assert_eq!(
        std::fs::read_to_string(&defaults).unwrap(),
        format!("x-scheme-handler/telnet={GENERATED_DESKTOP_ID}\n")
    );
    run(backend.shutdown());
}

#[cfg(target_os = "linux")]
fn run<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(future)
}
