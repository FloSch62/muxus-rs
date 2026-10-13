//! The host editor dialog (`HostEditorDialog.tsx`): one view owning the
//! drafts of every connection kind, so switching the type while creating a
//! host swaps the form but never loses what was already typed.

mod desktop;
pub mod form;
mod management;
mod native;
pub mod shared;
mod ssh;
pub mod ssh_access;
mod terminal;

pub use terminal::{custom_schemes, scheme_select, terminal_color_defaults};

use std::time::Duration;

use muxus_core::api::{
    HostKeywordHighlightConfig, SavedHostKind, SessionLoggingPolicy, SshKeysResponse,
};
use muxus_session::Backend;
use muxus_session::login_sequence::editor::LoginSequenceDraft;
use muxus_store::prefs::state::NewSshHostStorage;
use muxus_ui::gpui_kit::{
    AnyElement, App, AppContext, Context, Entity, EventEmitter, FocusHandle, Focusable,
    InteractiveElement, IntoElement, KeyDownEvent, ParentElement, Render, ScrollHandle,
    SharedString, Styled, Task, Window, div,
};
use muxus_ui::theme::{ActiveTheme, Status};
use muxus_ui::widgets::dialog::modal;

use super::logic::desktop_draft::{
    DesktopHostDraft, DesktopKind, blank_desktop_draft, desktop_draft_from_profile,
    desktop_draft_problem,
};
use super::logic::draft::{
    HostDraft, Storage, blank_draft, draft_from_entry, draft_from_saved_ssh_profile, draft_problem,
    identity_agent_for_detection,
};
use super::logic::forwards::ForwardRuleDraft;
use super::logic::management_draft::{
    ManagementHostDraft, ManagementKind, blank_management_draft, management_draft_from_profile,
    management_draft_problem,
};
use super::logic::native_draft::{
    NativeHostDraft, NativeKind, blank_native_draft, native_draft_from_profile,
    native_draft_problem,
};
use super::logic::session_logging::{HostSessionLoggingDraft, host_session_logging_draft};
use super::services::{self, ConnectTarget, EditorData};
use super::widgets::inputs::{InputCache, Menus};
use super::widgets::shell::{SectionTab, ShellProps, confirm_paper, editor_shell};
use crate::hosts::managed::ManagedHost;
use form::Form;

/// What the editor opens on (`HostEditorState` without `false`).
#[derive(Debug, Clone, PartialEq)]
pub enum HostEditorTarget {
    New(NewHost),
    /// Edit an ssh_config host (`edit`) or a saved host (`edit-profile`).
    Edit(ManagedHost),
    /// Start a new host from an existing one.
    Duplicate(ManagedHost),
}

/// The "Add host" variants: a quick-connect target, a folder, a kind.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewHost {
    pub prefill_target: Option<String>,
    /// Name for the new host, e.g. when adding a gNMI host for an SSH one.
    pub prefill_name: Option<String>,
    pub group: Option<String>,
    pub kind: Option<SavedHostKind>,
}

/// What the editor tells its owner.
#[derive(Debug, Clone, PartialEq)]
pub enum HostEditorEvent {
    /// Close the dialog (Cancel, Escape, the backdrop, after a delete).
    Close,
    /// The host was saved; the dialog is done. `connect` is set for "Save & connect".
    Saved { connect: Option<ConnectTarget> },
    /// The host was deleted.
    Deleted,
    /// Something to show in a toast (save errors).
    Toast { status: Status, message: String },
}

/// The sections of every editor kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Section {
    General,
    Appearance,
    Auth,
    Route,
    Forwards,
    Login,
    Logging,
    Highlighting,
    Advanced,
    Line,
    Logon,
    Security,
    Options,
}

impl Section {
    pub const ALL: [Section; 13] = [
        Section::General,
        Section::Appearance,
        Section::Auth,
        Section::Route,
        Section::Forwards,
        Section::Login,
        Section::Logging,
        Section::Highlighting,
        Section::Advanced,
        Section::Line,
        Section::Logon,
        Section::Security,
        Section::Options,
    ];

    pub fn id(self) -> &'static str {
        match self {
            Section::General => "general",
            Section::Appearance => "appearance",
            Section::Auth => "auth",
            Section::Route => "route",
            Section::Forwards => "forwards",
            Section::Login => "login",
            Section::Logging => "logging",
            Section::Highlighting => "highlighting",
            Section::Advanced => "advanced",
            Section::Line => "line",
            Section::Logon => "logon",
            Section::Security => "security",
            Section::Options => "options",
        }
    }

    pub fn parse(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|section| section.id() == id)
    }
}

/// The host editor.
pub struct HostEditor {
    pub(super) backend: Backend,
    pub(super) target: HostEditorTarget,
    pub(super) kind: SavedHostKind,
    pub(super) ssh: HostDraft,
    pub(super) native: NativeHostDraft,
    pub(super) desktop: DesktopHostDraft,
    pub(super) management: ManagementHostDraft,
    pub(super) section: Section,
    pub(super) data: EditorData,
    pub(super) data_loaded: bool,
    /// An X11 status that wins over the backend's (screenshots of the demo).
    x11_pinned: Option<muxus_core::api::X11Status>,
    pub(super) keys: Option<SshKeysResponse>,
    keys_agent: Option<Option<String>>,
    _keys_task: Option<Task<()>>,
    pub(super) serial_ports: Option<Result<Vec<muxus_core::api::SerialPortInfo>, String>>,
    pub(super) serial_fetching: bool,
    pub(super) preview: String,
    pub(super) preview_error: Option<String>,
    preview_for: Option<muxus_core::api::HostUpsertRequest>,
    _preview_task: Option<Task<()>>,
    /// `savedProfile.current`: the profile this dialog saved, so a second save updates it.
    saved_profile: Option<muxus_core::api::SavedHostProfile>,
    pub(super) busy: bool,
    delete_pending: bool,
    confirm_delete: bool,
    pub(super) inputs: InputCache,
    pub(super) menus: Menus,
    pub(super) pending: ssh_access::SshPending,
    pub(super) new_file_name: String,
    pub(super) forward_rule: ForwardRuleDraft,
    /// Stable ids of the Advanced option rows, for their inputs.
    pub(super) extra_ids: Vec<u64>,
    pub(super) next_row_id: u64,
    /// The color input whose choices are open.
    pub(super) color_open: Option<&'static str>,
    focus: FocusHandle,
    pub(super) scroll: ScrollHandle,
    _tasks: Vec<Task<()>>,
}

impl EventEmitter<HostEditorEvent> for HostEditor {}

impl Focusable for HostEditor {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus.clone()
    }
}

impl ssh_access::SshForm for HostEditor {
    fn ssh_draft(&self) -> &HostDraft {
        &self.ssh
    }

    fn ssh_draft_mut(&mut self) -> &mut HostDraft {
        &mut self.ssh
    }

    fn ssh_changed(&mut self, cx: &mut Context<Self>) {
        self.changed(cx);
    }

    fn ssh_keys(&self) -> Option<&SshKeysResponse> {
        self.keys.as_ref()
    }

    fn ssh_data(&self) -> &EditorData {
        &self.data
    }

    fn self_profile_id(&self) -> Option<String> {
        self.editing_profile()
            .map(|p| p.id.clone())
            .or_else(|| self.saved_profile.as_ref().map(|p| p.id.clone()))
    }

    fn pending(&mut self) -> &mut ssh_access::SshPending {
        &mut self.pending
    }
}

impl Form for HostEditor {
    fn inputs(&mut self) -> &mut InputCache {
        &mut self.inputs
    }

    fn menus(&mut self) -> &mut Menus {
        &mut self.menus
    }

    fn vault_changed(&mut self, status: muxus_core::api::PasswordVaultStatus) {
        self.data.vault = Some(status);
    }
}

/// Open the host editor. The owner mounts the returned entity over the
/// window (it draws its own backdrop) and drops it on `Close`/`Saved`/`Deleted`.
pub fn open_host_editor(
    window: &mut Window,
    cx: &mut App,
    backend: Backend,
    target: HostEditorTarget,
) -> Entity<HostEditor> {
    cx.new(|cx| HostEditor::new(backend, target, window, cx))
}

fn editor_kind(target: &HostEditorTarget) -> SavedHostKind {
    match target {
        HostEditorTarget::New(new) => new.kind.unwrap_or(SavedHostKind::Ssh),
        HostEditorTarget::Edit(host) | HostEditorTarget::Duplicate(host) => match host {
            ManagedHost::Ssh(_) => SavedHostKind::Ssh,
            ManagedHost::Profile(entry) => entry.kind,
        },
    }
}

impl HostEditor {
    pub fn new(
        backend: Backend,
        target: HostEditorTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        let kind = editor_kind(&target);
        let mut editor = Self {
            backend: backend.clone(),
            kind,
            ssh: HostDraft::default(),
            native: NativeHostDraft::default(),
            desktop: DesktopHostDraft::default(),
            management: ManagementHostDraft::default(),
            section: Section::General,
            data: EditorData::default(),
            data_loaded: false,
            x11_pinned: None,
            keys: None,
            keys_agent: None,
            _keys_task: None,
            serial_ports: None,
            serial_fetching: false,
            preview: String::new(),
            preview_error: None,
            preview_for: None,
            _preview_task: None,
            saved_profile: None,
            busy: false,
            delete_pending: false,
            confirm_delete: false,
            inputs: InputCache::default(),
            menus: Menus::default(),
            pending: ssh_access::SshPending::default(),
            new_file_name: String::new(),
            forward_rule: ForwardRuleDraft::default(),
            extra_ids: Vec::new(),
            next_row_id: 0,
            color_open: None,
            focus,
            scroll: ScrollHandle::new(),
            _tasks: Vec::new(),
            target: target.clone(),
        };
        editor.seed_drafts(NewSshHostStorage::Openssh);
        editor.load(cx);
        editor
    }

    /// The drafts for the target (`initialSshDraft` and friends).
    fn seed_drafts(&mut self, storage: NewSshHostStorage) {
        match &self.target {
            HostEditorTarget::New(new) => {
                let prefill = new.prefill_target.as_deref().unwrap_or("");
                let group = new.group.as_deref().unwrap_or("");
                self.ssh = blank_draft(
                    prefill,
                    group,
                    match storage {
                        NewSshHostStorage::Openssh => Storage::Openssh,
                        NewSshHostStorage::Muxus => Storage::Muxus,
                    },
                );
                self.native = blank_native_draft(prefill, group);
                self.desktop = blank_desktop_draft(prefill, group);
                self.management = blank_management_draft(prefill, group);
                if let Some(name) = &new.prefill_name
                    && !name.is_empty()
                {
                    self.management.name = name.clone();
                }
            }
            HostEditorTarget::Edit(host) | HostEditorTarget::Duplicate(host) => {
                let duplicate = matches!(self.target, HostEditorTarget::Duplicate(_));
                match host {
                    ManagedHost::Ssh(entry) => self.ssh = draft_from_entry(entry, duplicate),
                    ManagedHost::Profile(entry) => {
                        if let Some(draft) = draft_from_saved_ssh_profile(entry, duplicate) {
                            self.ssh = draft;
                        }
                        if let Some(draft) = native_draft_from_profile(entry, duplicate) {
                            self.native = draft;
                        }
                        if let Some(draft) = desktop_draft_from_profile(entry, duplicate) {
                            self.desktop = draft;
                        }
                        if let Some(draft) = management_draft_from_profile(entry, duplicate) {
                            self.management = draft;
                        }
                    }
                }
            }
        }
        self.extra_ids = (0..self.ssh.extras.len() as u64).collect();
        self.next_row_id = self.extra_ids.len() as u64;
    }

    /// Whether the target is an existing host being edited (`edit`/`edit-profile`).
    pub(super) fn editing(&self) -> bool {
        matches!(self.target, HostEditorTarget::Edit(_))
    }

    pub(super) fn is_new(&self) -> bool {
        matches!(self.target, HostEditorTarget::New(_))
    }

    /// The ssh_config entry being edited (`state.mode === 'edit'`).
    pub(super) fn editing_entry(&self) -> Option<&muxus_core::api::SshHostEntry> {
        match &self.target {
            HostEditorTarget::Edit(ManagedHost::Ssh(entry)) => Some(entry),
            _ => None,
        }
    }

    /// The saved host being edited (`state.mode === 'edit-profile'`).
    pub(super) fn editing_profile(&self) -> Option<&muxus_core::api::SavedHostProfile> {
        match &self.target {
            HostEditorTarget::Edit(ManagedHost::Profile(entry)) => Some(entry),
            _ => None,
        }
    }

    fn logging_policy_key(&self) -> String {
        match &self.target {
            HostEditorTarget::Edit(ManagedHost::Ssh(entry)) => format!("ssh:{}", entry.alias),
            HostEditorTarget::Edit(ManagedHost::Profile(entry)) => format!("profile:{}", entry.id),
            _ => "*".into(),
        }
    }

    fn load(&mut self, cx: &mut Context<Self>) {
        let backend = self.backend.clone();
        let key = self.logging_policy_key();
        let new = self.is_new();
        let task = cx.spawn(async move |this, cx| {
            let data = services::load_editor_data(&backend).await;
            let policy = services::logging_policy(&backend, &key).await;
            let _ = this.update(cx, |this, cx| {
                let storage = data.prefs.new_ssh_host_storage;
                this.data = data;
                if let Some(x11) = this.x11_pinned.clone() {
                    this.data.x11 = Some(x11);
                }
                this.data_loaded = true;
                if new && storage != NewSshHostStorage::Openssh {
                    // `usePrefsStore.getState().newSshHostStorage` decides where a new SSH host starts.
                    let keep = std::mem::take(&mut this.native);
                    this.seed_drafts(storage);
                    this.native = keep;
                    this.inputs.clear();
                }
                if let Some(policy) = policy {
                    this.apply_logging_policy(&policy);
                }
                this.refresh_keys(cx);
                this.refresh_preview(cx);
                cx.notify();
            });
        });
        self._tasks.push(task);
    }

    fn apply_logging_policy(&mut self, policy: &SessionLoggingPolicy) {
        let inherit = !self.editing() || !policy.overridden;
        let draft = host_session_logging_draft(policy, inherit);
        if !self.ssh.session_logging.loaded {
            self.ssh.session_logging = draft;
        }
        if !self.native.session_logging.loaded {
            self.native.session_logging = draft;
        }
    }

    /// The agent the key picker asks about follows the draft's agent source.
    pub(super) fn refresh_keys(&mut self, cx: &mut Context<Self>) {
        if self.kind != SavedHostKind::Ssh {
            return;
        }
        let inherited = match self.editing_entry() {
            Some(entry) if entry.options.identity_agent.is_none() => {
                entry.resolved.identity_agent.clone()
            }
            _ => None,
        };
        let agent = identity_agent_for_detection(
            self.ssh.identity_agent_mode,
            &self.ssh.identity_agent,
            inherited.as_deref(),
        );
        if self.keys_agent.as_ref() == Some(&agent) {
            return;
        }
        self.keys_agent = Some(agent.clone());
        let backend = self.backend.clone();
        self._keys_task = Some(cx.spawn(async move |this, cx| {
            let keys = backend.ssh_keys(agent.as_deref()).await.ok();
            let _ = this.update(cx, |this, cx| {
                this.keys = keys;
                cx.notify();
            });
        }));
    }

    /// Live preview of the exact block text, rendered by the backend (debounced).
    pub(super) fn refresh_preview(&mut self, cx: &mut Context<Self>) {
        if self.kind != SavedHostKind::Ssh
            || self.ssh.storage != Storage::Openssh
            || self.problem().is_some()
        {
            return;
        }
        let previous = self.editing_entry().map(|e| e.alias.clone());
        let request = super::logic::draft::draft_to_request(&self.ssh, previous.as_deref());
        if self.preview_for.as_ref() == Some(&request) {
            return;
        }
        self.preview_for = Some(request);
        let backend = self.backend.clone();
        let draft = self.ssh.clone();
        self._preview_task = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(350))
                .await;
            let result = services::preview_ssh_host(&backend, &draft, previous.as_deref()).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(text) => {
                        this.preview = text;
                        this.preview_error = None;
                    }
                    Err(error) => this.preview_error = Some(error),
                }
                cx.notify();
            });
        }));
    }

    /// Something in a draft changed.
    pub(super) fn changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_keys(cx);
        self.refresh_preview(cx);
        cx.notify();
    }

    pub(super) fn native_kind(&self) -> NativeKind {
        if self.kind == SavedHostKind::Serial {
            NativeKind::Serial
        } else {
            NativeKind::Telnet
        }
    }

    pub(super) fn desktop_kind(&self) -> DesktopKind {
        if self.kind == SavedHostKind::Vnc {
            DesktopKind::Vnc
        } else {
            DesktopKind::Rdp
        }
    }

    pub(super) fn management_kind(&self) -> ManagementKind {
        if self.kind == SavedHostKind::Netconf {
            ManagementKind::Netconf
        } else {
            ManagementKind::Gnmi
        }
    }

    /// The draft of a terminal kind has loaded its logging policy.
    fn loading(&self) -> Option<String> {
        let loaded = match self.kind {
            SavedHostKind::Ssh => self.ssh.session_logging.loaded,
            SavedHostKind::Telnet | SavedHostKind::Serial => self.native.session_logging.loaded,
            _ => true,
        };
        (!loaded).then(|| "Loading session logging settings…".to_owned())
    }

    /// Why Save is blocked, for the active kind.
    pub(super) fn problem(&self) -> Option<String> {
        if self.loading().is_some() {
            return None;
        }
        match self.kind {
            SavedHostKind::Ssh => draft_problem(&self.ssh),
            SavedHostKind::Telnet | SavedHostKind::Serial => {
                native_draft_problem(&self.native, self.native_kind())
            }
            SavedHostKind::Rdp | SavedHostKind::Vnc => {
                desktop_draft_problem(&self.desktop, self.desktop_kind())
            }
            SavedHostKind::Gnmi | SavedHostKind::Netconf => {
                management_draft_problem(&self.management, self.management_kind())
            }
        }
    }

    fn title(&self) -> String {
        let name = |host: &ManagedHost| match host {
            ManagedHost::Ssh(entry) => entry.alias.clone(),
            ManagedHost::Profile(entry) => entry.name.clone(),
        };
        match &self.target {
            HostEditorTarget::New(_) => "Add host".into(),
            HostEditorTarget::Edit(host) => format!("Edit {}", name(host)),
            HostEditorTarget::Duplicate(host) => format!("Duplicate {}", name(host)),
        }
    }

    fn storage_caption(&self) -> String {
        match self.kind {
            SavedHostKind::Ssh if self.ssh.storage == Storage::Muxus => {
                "Saved in Muxus app data — ssh_config is unchanged".into()
            }
            SavedHostKind::Ssh => {
                let file = if !self.ssh.file.is_empty() {
                    self.ssh.file.clone()
                } else {
                    self.data
                        .ssh_config
                        .as_ref()
                        .map(|c| c.path.clone())
                        .unwrap_or_else(|| "~/.ssh/config".into())
                };
                format!("Saved to {}", services::shorten_ssh_path(&file))
            }
            _ => "Saved in Muxus app data".into(),
        }
    }

    /// The rail for the active kind.
    pub(super) fn sections(&self) -> Vec<SectionTab> {
        let tab = |s: Section, label: &str, icon: &'static str, count: Option<usize>| {
            SectionTab::new(s.id(), label, icon, count)
        };
        let appearance = |scheme: &Option<String>,
                          font: &Option<String>,
                          bg: &Option<String>,
                          group: &Option<String>,
                          paste: bool| {
            let count = [
                scheme.is_some(),
                font.is_some(),
                bg.is_some(),
                group.is_some(),
                paste,
            ]
            .iter()
            .filter(|v| **v)
            .count();
            (count > 0).then_some(count)
        };
        let login = |draft: &LoginSequenceDraft| {
            (draft.mode == muxus_session::login_sequence::editor::LoginSequenceMode::Custom)
                .then_some(draft.steps.len())
        };
        let highlighting = |h: &HostKeywordHighlightConfig| {
            Some(h.rules.len() + usize::from(h.profile_id.is_some()))
        };
        match self.kind {
            SavedHostKind::Ssh => {
                let d = &self.ssh;
                vec![
                    tab(Section::General, "General", "DnsOutlined", None),
                    tab(
                        Section::Appearance,
                        "Terminal appearance",
                        "PaletteOutlined",
                        appearance(
                            &d.terminal_scheme,
                            &d.terminal_font_color,
                            &d.terminal_background_color,
                            &d.command_button_group,
                            d.paste_line_delay_ms.is_some() || d.paste_char_delay_ms.is_some(),
                        ),
                    ),
                    tab(Section::Auth, "Authentication", "KeyOutlined", None),
                    tab(
                        Section::Route,
                        "Connection route",
                        "AltRoute",
                        match d.route_mode {
                            super::logic::draft::RouteMode::Jump => Some(d.proxy_jump.len()),
                            super::logic::draft::RouteMode::Command => Some(1),
                            super::logic::draft::RouteMode::Direct => None,
                        },
                    ),
                    tab(
                        Section::Forwards,
                        "Forwarding",
                        "SwapHorizOutlined",
                        Some(d.forwards.len()),
                    ),
                    tab(
                        Section::Login,
                        "Login sequence",
                        "LoginOutlined",
                        login(&d.login_sequence),
                    ),
                    tab(Section::Logging, "Session logging", "HistoryOutlined", None),
                    tab(
                        Section::Highlighting,
                        "Highlighting",
                        "HighlightOutlined",
                        highlighting(&d.keyword_highlights),
                    ),
                    tab(
                        Section::Advanced,
                        "Advanced",
                        "CodeOutlined",
                        Some(
                            d.extras.len()
                                + usize::from(d.disable_sftp)
                                + usize::from(d.console_compatibility),
                        ),
                    ),
                ]
            }
            SavedHostKind::Telnet | SavedHostKind::Serial => {
                let d = &self.native;
                let serial = self.kind == SavedHostKind::Serial;
                let mut sections = vec![
                    tab(
                        Section::General,
                        "General",
                        if serial {
                            "UsbOutlined"
                        } else {
                            "LanguageOutlined"
                        },
                        None,
                    ),
                    tab(
                        Section::Appearance,
                        "Terminal appearance",
                        "PaletteOutlined",
                        appearance(
                            &d.terminal_scheme,
                            &d.terminal_font_color,
                            &d.terminal_background_color,
                            &d.command_button_group,
                            d.paste_line_delay_ms.is_some() || d.paste_char_delay_ms.is_some(),
                        ),
                    ),
                ];
                if serial {
                    sections.push(tab(Section::Line, "Line settings", "TuneOutlined", None));
                }
                sections.push(tab(
                    Section::Login,
                    "Login sequence",
                    "LoginOutlined",
                    login(&d.login_sequence),
                ));
                sections.push(tab(
                    Section::Logging,
                    "Session logging",
                    "HistoryOutlined",
                    None,
                ));
                sections.push(tab(
                    Section::Highlighting,
                    "Highlighting",
                    "HighlightOutlined",
                    highlighting(&d.keyword_highlights),
                ));
                sections
            }
            SavedHostKind::Rdp | SavedHostKind::Vnc => vec![
                tab(
                    Section::General,
                    "General",
                    if self.kind == SavedHostKind::Rdp {
                        "DesktopWindowsOutlined"
                    } else {
                        "ScreenShareOutlined"
                    },
                    None,
                ),
                tab(Section::Logon, "Logon", "KeyOutlined", None),
                tab(
                    Section::Route,
                    "Connection route",
                    "AltRoute",
                    self.desktop.ssh_gateway.as_ref().map(|_| 1),
                ),
                tab(Section::Options, "Options", "TuneOutlined", None),
            ],
            SavedHostKind::Gnmi | SavedHostKind::Netconf => {
                let gnmi = self.kind == SavedHostKind::Gnmi;
                let mut sections = vec![
                    tab(
                        Section::General,
                        "General",
                        if gnmi {
                            "SensorsOutlined"
                        } else {
                            "AccountTreeOutlined"
                        },
                        None,
                    ),
                    tab(Section::Login, "Login", "KeyOutlined", None),
                ];
                if gnmi {
                    sections.push(tab(Section::Security, "Security", "ShieldOutlined", None));
                }
                sections.push(tab(
                    Section::Route,
                    "Connection route",
                    "AltRoute",
                    self.management.ssh_gateway.as_ref().map(|_| 1),
                ));
                if gnmi {
                    sections.push(tab(Section::Options, "Options", "TuneOutlined", None));
                }
                sections
            }
        }
    }

    /// The section shown: one the rail lists, else General (a type switch
    /// can take the current one away).
    pub(super) fn visible_section(&self) -> Section {
        if self
            .sections()
            .iter()
            .any(|tab| tab.id == self.section.id())
        {
            self.section
        } else {
            Section::General
        }
    }

    /// Show this X11 status instead of the backend's (the docs' demo shows
    /// the bundled X server of the Windows app).
    pub fn pin_x11_status(&mut self, status: muxus_core::api::X11Status) {
        self.data.x11 = Some(status.clone());
        self.x11_pinned = Some(status);
    }

    pub fn set_section(&mut self, section: Section, cx: &mut Context<Self>) {
        self.section = section;
        self.menus.close();
        cx.notify();
    }

    /// Switch the connection type while creating.
    pub fn set_kind(&mut self, kind: SavedHostKind, cx: &mut Context<Self>) {
        if self.kind == kind {
            return;
        }
        // Each kind's content is its own component in the client, so its
        // section starts over; the drafts survive.
        let family = |k: SavedHostKind| match k {
            SavedHostKind::Ssh => 0,
            SavedHostKind::Telnet | SavedHostKind::Serial => 1,
            SavedHostKind::Rdp | SavedHostKind::Vnc => 2,
            SavedHostKind::Gnmi | SavedHostKind::Netconf => 3,
        };
        if family(kind) != family(self.kind) {
            self.section = Section::General;
        }
        self.kind = kind;
        self.menus.close();
        self.changed(cx);
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        cx.emit(HostEditorEvent::Close);
    }

    fn fail(&mut self, message: String, cx: &mut Context<Self>) {
        self.busy = false;
        self.delete_pending = false;
        cx.emit(HostEditorEvent::Toast {
            status: Status::Error,
            message,
        });
        cx.notify();
    }

    /// Save, then close (and connect with `connect`).
    pub fn save(&mut self, connect: bool, cx: &mut Context<Self>) {
        if self.busy || self.problem().is_some() || self.loading().is_some() {
            return;
        }
        self.busy = true;
        cx.notify();
        let backend = self.backend.clone();
        let kind = self.kind;
        match kind {
            SavedHostKind::Ssh => {
                let draft = self.ssh.clone();
                let previous = self.editing_entry().map(|e| e.alias.clone());
                let existing = self
                    .editing_profile()
                    .map(|p| p.id.clone())
                    .or_else(|| self.saved_profile.as_ref().map(|p| p.id.clone()));
                let task = cx.spawn(async move |this, cx| {
                    let result = services::save_ssh_host(
                        &backend,
                        &draft,
                        previous.as_deref(),
                        existing.as_deref(),
                    )
                    .await;
                    let _ = this.update(cx, |this, cx| match result {
                        Ok((target, profile)) => {
                            this.saved_profile = profile;
                            this.busy = false;
                            let connect = connect.then_some(target).filter(|t| match t {
                                ConnectTarget::Alias(alias) => !alias.is_empty(),
                                ConnectTarget::SavedHost(_) => true,
                            });
                            cx.emit(HostEditorEvent::Saved { connect });
                        }
                        Err(message) => this.fail(message, cx),
                    });
                });
                self._tasks.push(task);
            }
            _ => {
                let existing = self.editing_profile().map(|p| p.id.clone());
                let native = (self.native.clone(), self.native_kind());
                let desktop = (self.desktop.clone(), self.desktop_kind());
                let management = (self.management.clone(), self.management_kind());
                let task = cx.spawn(async move |this, cx| {
                    let result = match kind {
                        SavedHostKind::Telnet | SavedHostKind::Serial => {
                            services::save_native_host(
                                &backend,
                                &native.0,
                                native.1,
                                existing.as_deref(),
                            )
                            .await
                        }
                        SavedHostKind::Rdp | SavedHostKind::Vnc => {
                            services::save_desktop_host(
                                &backend,
                                &desktop.0,
                                desktop.1,
                                existing.as_deref(),
                            )
                            .await
                        }
                        _ => {
                            services::save_management_host(
                                &backend,
                                &management.0,
                                management.1,
                                existing.as_deref(),
                            )
                            .await
                        }
                    };
                    let _ = this.update(cx, |this, cx| match result {
                        Ok(profile) => {
                            this.busy = false;
                            this.saved_profile = Some(profile.clone());
                            cx.emit(HostEditorEvent::Saved {
                                connect: connect
                                    .then(|| ConnectTarget::SavedHost(Box::new(profile))),
                            });
                        }
                        Err(message) => this.fail(message, cx),
                    });
                });
                self._tasks.push(task);
            }
        }
    }

    fn delete(&mut self, cx: &mut Context<Self>) {
        let HostEditorTarget::Edit(host) = &self.target else {
            return;
        };
        let host = host.clone();
        self.confirm_delete = false;
        self.delete_pending = true;
        cx.notify();
        let backend = self.backend.clone();
        let task = cx.spawn(async move |this, cx| {
            let result = services::delete_host(&backend, &host).await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.delete_pending = false;
                    cx.emit(HostEditorEvent::Deleted);
                    cx.emit(HostEditorEvent::Close);
                }
                Err(message) => this.fail(message, cx),
            });
        });
        self._tasks.push(task);
    }

    fn on_key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "escape" => {
                cx.stop_propagation();
                if self.menus.open.is_some() || self.color_open.is_some() {
                    self.menus.close();
                    self.color_open = None;
                    cx.notify();
                } else if self.confirm_delete {
                    self.confirm_delete = false;
                    cx.notify();
                } else {
                    self.close(cx);
                }
            }
            "enter" if self.confirm_delete => {
                cx.stop_propagation();
                self.delete(cx);
            }
            "tab" => {
                cx.stop_propagation();
                if event.keystroke.modifiers.shift {
                    window.focus_prev(cx);
                } else {
                    window.focus_next(cx);
                }
            }
            _ => {}
        }
    }

    fn content(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let section = self.visible_section();
        match self.kind {
            SavedHostKind::Ssh => match section {
                Section::General => self.ssh_general(window, cx),
                Section::Appearance => self.appearance_section(window, cx),
                Section::Auth => self.ssh_auth(window, cx),
                Section::Route => self.ssh_route(window, cx),
                Section::Forwards => self.ssh_forwards(window, cx),
                Section::Login => self.login_section(window, cx),
                Section::Logging => self.logging_section(window, cx),
                Section::Highlighting => self.highlighting_section(window, cx),
                _ => self.ssh_advanced(window, cx),
            },
            SavedHostKind::Telnet | SavedHostKind::Serial => match section {
                Section::Appearance => self.appearance_section(window, cx),
                Section::Line => self.native_line(window, cx),
                Section::Login => self.login_section(window, cx),
                Section::Logging => self.logging_section(window, cx),
                Section::Highlighting => self.highlighting_section(window, cx),
                _ => self.native_general(window, cx),
            },
            SavedHostKind::Rdp | SavedHostKind::Vnc => match section {
                Section::Logon => self.desktop_logon(window, cx),
                Section::Route => self.desktop_route(window, cx),
                Section::Options => self.desktop_options(window, cx),
                _ => self.desktop_general(window, cx),
            },
            SavedHostKind::Gnmi | SavedHostKind::Netconf => match section {
                Section::Login => self.management_login(window, cx),
                Section::Security => self.management_security(window, cx),
                Section::Route => self.management_route(window, cx),
                Section::Options => self.management_options(window, cx),
                _ => self.management_general(window, cx),
            },
        }
    }
}

impl Render for HostEditor {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = *cx.theme();
        let content = self.content(window, cx);
        let entity = cx.entity().downgrade();
        let (close, section, kind, save, delete) = (
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
            entity.clone(),
        );
        let deletable = self.editing();
        let props = ShellProps {
            title: self.title().into(),
            storage: self.storage_caption().into(),
            kind: self.is_new().then_some(self.kind),
            on_kind: self.is_new().then(|| {
                std::rc::Rc::new(move |next: SavedHostKind, _: &mut Window, cx: &mut App| {
                    let _ = kind.update(cx, |this, cx| this.set_kind(next, cx));
                }) as super::widgets::shell::KindHandler
            }),
            sections: self.sections(),
            section: SharedString::from(self.visible_section().id()),
            on_section: std::rc::Rc::new(move |id: SharedString, _, cx: &mut App| {
                let _ = section.update(cx, |this, cx| {
                    if let Some(section) = Section::parse(&id) {
                        this.set_section(section, cx);
                    }
                });
            }),
            problem: self.problem(),
            loading: self.loading(),
            busy: self.busy,
            on_delete: deletable.then(|| {
                std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = delete.update(cx, |this, cx| {
                        this.confirm_delete = true;
                        cx.notify();
                    });
                }) as std::rc::Rc<dyn Fn(&mut Window, &mut App)>
            }),
            delete_pending: self.delete_pending,
            on_close: std::rc::Rc::new(move |_, cx: &mut App| {
                let _ = close.update(cx, |this, cx| this.close(cx));
            }),
            on_save: std::rc::Rc::new(move |connect, _, cx: &mut App| {
                let _ = save.update(cx, |this, cx| this.save(connect, cx));
            }),
            save_label: "Save".into(),
            connect_action: true,
            scroll: self.scroll.clone(),
        };
        let backdrop_close = entity.clone();
        let mut root = div()
            .id("host-editor")
            .absolute()
            .inset_0()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(Self::on_key_down))
            .child(modal(
                editor_shell(&theme, window, props, content),
                Some(std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = backdrop_close.update(cx, |this, cx| this.close(cx));
                })),
            ));
        if self.confirm_delete
            && let HostEditorTarget::Edit(host) = &self.target
        {
            let (title, description) = services::delete_question(host);
            let (cancel, confirm, dismiss) = (entity.clone(), entity.clone(), entity);
            root = root.child(modal(
                confirm_paper(
                    &theme,
                    title,
                    description,
                    "Delete",
                    std::rc::Rc::new(move |_, cx: &mut App| {
                        let _ = cancel.update(cx, |this, cx| {
                            this.confirm_delete = false;
                            cx.notify();
                        });
                    }),
                    std::rc::Rc::new(move |_, cx: &mut App| {
                        let _ = confirm.update(cx, |this, cx| this.delete(cx));
                    }),
                ),
                Some(std::rc::Rc::new(move |_: &mut Window, cx: &mut App| {
                    let _ = dismiss.update(cx, |this, cx| {
                        this.confirm_delete = false;
                        cx.notify();
                    });
                })),
            ));
        }
        root
    }
}

/// Logging draft accessor shared by the terminal sections.
pub(super) fn logging_draft(editor: &mut HostEditor) -> &mut HostSessionLoggingDraft {
    if editor.kind == SavedHostKind::Ssh {
        &mut editor.ssh.session_logging
    } else {
        &mut editor.native.session_logging
    }
}
