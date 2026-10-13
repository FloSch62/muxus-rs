//! The folder dialog's form (`client/src/components/sidebar/FolderDialog.tsx`):
//! create, rename, re-parent and style a sidebar folder, give it shared SSH
//! credentials and a login sequence, or pick the folder hosts move to. All of
//! them are a rewrite of one group path.

use muxus_core::api::{FolderAuthSettings, FolderSettingsRecord, LoginSequence};
use muxus_core::js::js_trim;
use muxus_session::login_sequence::editor::{
    LoginSequenceDraft, inherited_login_sequence, login_sequence_draft, login_sequence_from_draft,
    login_sequence_problem, same_login_sequence,
};

use crate::hosts::managed::{ManagedHost, managed_host_key};
use crate::hosts::tree::{
    folder_key, folder_label, folder_parent_path, folder_path, folder_segments, is_same_path,
    normalize_group_path, sanitize_folder_name,
};
use crate::model::FolderStyle;
use crate::sidebar::folder_mutations::{
    FolderMove, FolderProblem, folder_problem_message, folder_rewrite_plan, folder_target_problem,
    move_hosts_plan,
};
use crate::sidebar::folder_prefs::FolderPrefs;

/// What the folder dialog opens on (`FolderDialogState`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderDialogState {
    /// Create a folder, optionally already nested under `parent_path`.
    New { parent_path: Option<String> },
    /// Rename, re-parent, colour or icon an existing folder.
    Edit { path: String },
    /// Pick the folder hosts should live in. `current_path` is the folder they
    /// all share, or empty when they are spread across several.
    MoveHosts {
        host_keys: Vec<String>,
        host_name: Option<String>,
        current_path: String,
    },
}

/// Everything the form holds.
#[derive(Debug, Clone, PartialEq)]
pub struct FolderDraft {
    pub name: String,
    pub parent: String,
    pub color: Option<String>,
    pub icon: Option<String>,
    pub auth_user: String,
    pub auth_port: String,
    pub auth_key: String,
    pub auth_password: String,
    pub remove_password: bool,
    pub master_password: String,
    pub login_sequence: LoginSequenceDraft,
}

/// The folder's stored settings record, matched case-insensitively.
pub fn folder_settings_for_path<'a>(
    folders: &'a [FolderSettingsRecord],
    path: &str,
) -> Option<&'a FolderSettingsRecord> {
    folders
        .iter()
        .find(|record| is_same_path(&record.path, path))
}

impl FolderDraft {
    /// The form as it opens: the folder's name, parent and style; the
    /// credential fields from its settings record when editing.
    pub fn new(
        state: &FolderDialogState,
        prefs: &FolderPrefs,
        settings: Option<&FolderSettingsRecord>,
    ) -> Self {
        let mut draft = FolderDraft {
            name: String::new(),
            parent: String::new(),
            color: None,
            icon: None,
            auth_user: String::new(),
            auth_port: String::new(),
            auth_key: String::new(),
            auth_password: String::new(),
            remove_password: false,
            master_password: String::new(),
            login_sequence: login_sequence_draft(None),
        };
        match state {
            FolderDialogState::MoveHosts { current_path, .. } => {
                draft.parent = current_path.clone()
            }
            FolderDialogState::New { parent_path } => {
                draft.parent = parent_path.clone().unwrap_or_default();
            }
            FolderDialogState::Edit { path } => {
                let style = prefs.folder_style(&folder_key(path));
                draft.name = folder_label(path);
                draft.parent = folder_parent_path(path);
                draft.color = style.and_then(|s| s.color.clone());
                draft.icon = style.and_then(|s| s.icon.clone());
                draft.seed_credentials(settings);
            }
        }
        draft
    }

    /// Seed the credential fields separately: the settings arrive from their
    /// own query and may land a beat after the dialog opens.
    pub fn seed_credentials(&mut self, settings: Option<&FolderSettingsRecord>) {
        self.auth_user = settings
            .and_then(|s| s.auth.user.clone())
            .unwrap_or_default();
        self.auth_port = settings
            .and_then(|s| s.auth.port)
            .map(|p| p.to_string())
            .unwrap_or_default();
        self.auth_key = settings
            .and_then(|s| s.auth.identity_files.as_ref())
            .and_then(|files| files.first().cloned())
            .unwrap_or_default();
        self.auth_password.clear();
        self.remove_password = false;
        self.master_password.clear();
        self.login_sequence =
            login_sequence_draft(settings.and_then(|s| s.login_sequence.as_ref()));
    }
}

/// The source folder of an edit, or empty.
pub fn source_path(state: &FolderDialogState) -> &str {
    match state {
        FolderDialogState::Edit { path } => path,
        _ => "",
    }
}

/// The path the dialog writes to.
pub fn target_path(state: &FolderDialogState, draft: &FolderDraft) -> String {
    if matches!(state, FolderDialogState::MoveHosts { .. }) {
        return normalize_group_path(Some(&draft.parent));
    }
    let mut segments = folder_segments(Some(&draft.parent));
    segments.push(sanitize_folder_name(&draft.name));
    normalize_group_path(Some(&folder_path(&segments)))
}

/// Why the folder cannot be created or moved there (not for moving hosts).
pub fn target_problem(state: &FolderDialogState, draft: &FolderDraft) -> Option<FolderProblem> {
    if matches!(state, FolderDialogState::MoveHosts { .. }) {
        return None;
    }
    folder_target_problem(source_path(state), &target_path(state, draft))
}

/// Exact comparison, not folder identity: two paths that differ only in case
/// are the same folder, but changing its capitalisation is still a rename and
/// has to be written out, or the old spelling stays on screen.
pub fn renaming(state: &FolderDialogState, draft: &FolderDraft) -> bool {
    match state {
        FolderDialogState::Edit { path } => {
            !path.is_empty() && target_path(state, draft) != normalize_group_path(Some(path))
        }
        _ => false,
    }
}

/// Whether a rename lands on another folder that already exists.
pub fn merging(state: &FolderDialogState, draft: &FolderDraft, known_paths: &[String]) -> bool {
    let target = target_path(state, draft);
    let source = source_path(state);
    renaming(state, draft)
        && known_paths
            .iter()
            // Recapitalising a folder lands on itself, which is not a merge.
            .any(|path| is_same_path(path, &target) && !is_same_path(path, source))
}

/// The hosts a rename moves.
pub fn affected_hosts(
    state: &FolderDialogState,
    draft: &FolderDraft,
    hosts: &[ManagedHost],
) -> Vec<FolderMove> {
    if !renaming(state, draft) {
        return Vec::new();
    }
    folder_rewrite_plan(hosts, source_path(state), &target_path(state, draft))
}

/// The line under the fields when a rename moves hosts.
pub fn rename_note(target: &str, affected: usize, merging: bool) -> Option<String> {
    if affected == 0 {
        return None;
    }
    let hosts = format!("{affected} host{}", if affected == 1 { "" } else { "s" });
    Some(if merging {
        format!(
            "Merges into the existing “{}” and moves {hosts}.",
            folder_label(target)
        )
    } else {
        format!("Moves {hosts}.")
    })
}

/// The shared port as typed: `None` when empty, `Some(Err)` when invalid.
pub fn port_number(draft: &FolderDraft) -> Option<Result<u16, ()>> {
    let text = js_trim(&draft.auth_port);
    if text.is_empty() {
        return None;
    }
    let value = super::js::js_number(text);
    Some(
        if value.is_finite() && value.fract() == 0. && (1. ..=65_535.).contains(&value) {
            Ok(value as u16)
        } else {
            Err(())
        },
    )
}

pub fn port_invalid(draft: &FolderDraft) -> bool {
    matches!(port_number(draft), Some(Err(())))
}

/// The vault's state as far as the password field cares.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VaultState {
    pub configured: bool,
    pub locked: bool,
}

pub fn password_needs_master(draft: &FolderDraft, vault: VaultState) -> bool {
    vault.configured && vault.locked && !draft.auth_password.is_empty()
}

/// Why Save stays disabled, beyond a pending write.
pub fn submit_blocked(state: &FolderDialogState, draft: &FolderDraft, vault: VaultState) -> bool {
    if matches!(state, FolderDialogState::MoveHosts { .. }) {
        return false;
    }
    target_problem(state, draft).is_some()
        || port_invalid(draft)
        || login_sequence_problem(&draft.login_sequence).is_some()
        || (password_needs_master(draft, vault) && js_trim(&draft.master_password).is_empty())
}

/// The message under the parent field, or its default helper text.
pub fn parent_helper_text(state: &FolderDialogState, draft: &FolderDraft) -> (bool, String) {
    match target_problem(state, draft) {
        Some(problem) if problem != FolderProblem::Empty => {
            (true, folder_problem_message(problem, "folder"))
        }
        _ => (
            false,
            match state {
                FolderDialogState::MoveHosts { host_keys, .. } => format!(
                    "Leave empty to move {} out of every folder.",
                    if host_keys.len() == 1 {
                        "this host"
                    } else {
                        "these hosts"
                    }
                ),
                _ => "Leave empty for a top-level folder.".into(),
            },
        ),
    }
}

pub fn name_helper_text(name: &str) -> &'static str {
    if name.contains('/') {
        "A name cannot contain “/” — use the field below to nest."
    } else {
        "The name shown in the sidebar."
    }
}

pub fn password_helper_text(
    draft: &FolderDraft,
    vault: VaultState,
    has_password: bool,
) -> &'static str {
    if !vault.configured {
        "Set up the password vault in Settings → Passwords to store a shared password."
    } else if draft.remove_password {
        "The saved password is removed when you save."
    } else if has_password {
        "Leave empty to keep the saved password."
    } else {
        "Offered when a host falls back to password login; kept in the encrypted vault."
    }
}

pub fn title(state: &FolderDialogState) -> String {
    match state {
        FolderDialogState::MoveHosts {
            host_name: Some(name),
            ..
        } => format!("Move “{name}”"),
        FolderDialogState::MoveHosts { host_keys, .. } => format!("Move {} hosts", host_keys.len()),
        FolderDialogState::New { .. } => "New folder".into(),
        FolderDialogState::Edit { path } => format!("Edit “{}”", folder_label(path)),
    }
}

pub fn submit_label(state: &FolderDialogState) -> &'static str {
    match state {
        FolderDialogState::MoveHosts { .. } => "Move",
        FolderDialogState::New { .. } => "Create",
        FolderDialogState::Edit { .. } => "Save",
    }
}

/// The folder settings write a save performs (`PUT /api/folders/settings`).
#[derive(Debug, Clone, PartialEq)]
pub struct FolderSettingsWrite {
    pub path: String,
    pub auth: FolderAuthSettings,
    /// `None` keeps the stored password, `Some(None)` removes it, `Some(Some(p))` stores `p`.
    pub password: Option<Option<String>>,
    pub master_password: Option<String>,
    /// Left out when unchanged, so a concurrent edit elsewhere is not overwritten.
    pub login_sequence: Option<Option<LoginSequence>>,
}

/// Write the credential fields for `path`; `None` when there is nothing to
/// store and nothing stored before.
pub fn credentials_write(
    path: &str,
    draft: &FolderDraft,
    record: Option<&FolderSettingsRecord>,
) -> Option<FolderSettingsWrite> {
    let user = js_trim(&draft.auth_user);
    let key = js_trim(&draft.auth_key);
    let auth = FolderAuthSettings {
        user: (!user.is_empty()).then(|| user.to_owned()),
        port: port_number(draft).and_then(Result::ok),
        // A folder key means "log in with exactly this key", like the host
        // editor's specific-key mode.
        identity_files: (!key.is_empty()).then(|| vec![key.to_owned()]),
        identities_only: (!key.is_empty()).then_some(true),
        ..FolderAuthSettings::default()
    };
    let password = if !draft.auth_password.is_empty() {
        Some(Some(draft.auth_password.clone()))
    } else if draft.remove_password && record.is_some_and(|r| r.has_password) {
        Some(None)
    } else {
        None
    };
    let sequence = login_sequence_from_draft(&draft.login_sequence);
    let dirty = record.is_some() || !auth.is_empty() || password.is_some() || sequence.is_some();
    if !dirty {
        return None;
    }
    let master = js_trim(&draft.master_password);
    let unchanged = same_login_sequence(
        sequence.as_ref(),
        record.and_then(|r| r.login_sequence.as_ref()),
    );
    Some(FolderSettingsWrite {
        path: path.to_owned(),
        auth,
        password,
        master_password: (!master.is_empty()).then(|| master.to_owned()),
        login_sequence: (!unchanged).then_some(sequence),
    })
}

/// The login sequence a host in the folder inherits from the folder's parents.
pub fn inherited_sequence(
    folders: &[FolderSettingsRecord],
    target: &str,
) -> Option<(LoginSequence, String)> {
    inherited_login_sequence(Some(folders), Some(&folder_parent_path(target)))
}

/// Local preference edits a save makes, applied by whoever owns the prefs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FolderPrefsChange {
    AddEmptyFolder(String),
    RemoveEmptyFolder(String),
    SetStyle {
        key: String,
        style: Option<FolderStyle>,
    },
    Rename {
        from: String,
        to: String,
    },
}

impl FolderPrefsChange {
    pub fn apply(&self, prefs: &mut FolderPrefs) {
        match self {
            FolderPrefsChange::AddEmptyFolder(path) => prefs.add_empty_folder(path),
            FolderPrefsChange::RemoveEmptyFolder(path) => prefs.remove_empty_folder(path),
            FolderPrefsChange::SetStyle { key, style } => {
                prefs.set_folder_style(key, style.clone())
            }
            FolderPrefsChange::Rename { from, to } => prefs.rename_folder_prefs(from, to),
        }
    }
}

/// Everything one submit does.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FolderSubmit {
    /// Prefs edits, in order.
    pub prefs: Vec<FolderPrefsChange>,
    /// Host group rewrites (metadata patches).
    pub moves: Vec<FolderMove>,
    /// The folder the moves name, for the toast (a renamed folder).
    pub moves_label: Option<String>,
    /// A settings record to move before the credential write: `(from, to)`.
    pub move_settings: Option<(String, String)>,
    pub credentials: Option<FolderSettingsWrite>,
    pub toast: Option<String>,
}

/// The writes a submit performs, or `None` when Save is blocked.
pub fn plan_submit(
    state: &FolderDialogState,
    draft: &FolderDraft,
    hosts: &[ManagedHost],
    record: Option<&FolderSettingsRecord>,
    vault: VaultState,
) -> Option<FolderSubmit> {
    let target = target_path(state, draft);
    let style = |key: String| FolderPrefsChange::SetStyle {
        key,
        style: Some(FolderStyle {
            color: draft.color.clone(),
            icon: draft.icon.clone(),
        }),
    };
    match state {
        FolderDialogState::MoveHosts { host_keys, .. } => {
            let moving: Vec<ManagedHost> = hosts
                .iter()
                .filter(|host| host_keys.contains(&managed_host_key(host)))
                .cloned()
                .collect();
            Some(FolderSubmit {
                moves: move_hosts_plan(&moving, &target),
                ..FolderSubmit::default()
            })
        }
        _ if submit_blocked(state, draft, vault) => None,
        FolderDialogState::New { .. } => Some(FolderSubmit {
            prefs: vec![
                FolderPrefsChange::AddEmptyFolder(target.clone()),
                style(folder_key(&target)),
            ],
            credentials: credentials_write(&target, draft, None),
            ..FolderSubmit::default()
        }),
        FolderDialogState::Edit { path } => {
            let renamed = renaming(state, draft);
            // Style first: it is local and instant, so the folder keeps its look
            // while the hosts underneath it are still moving.
            let mut submit = FolderSubmit {
                prefs: vec![style(folder_key(if renamed { &target } else { path }))],
                ..FolderSubmit::default()
            };
            if renamed {
                submit.prefs.push(FolderPrefsChange::Rename {
                    from: path.clone(),
                    to: target.clone(),
                });
                // The rename already carried the empty-folder markers over.
                // Clearing the old path is only safe when it is a different
                // folder: paths match case-insensitively, so after a
                // recapitalisation it would take the marker it just rewrote.
                if !is_same_path(path, &target) {
                    submit
                        .prefs
                        .push(FolderPrefsChange::RemoveEmptyFolder(path.clone()));
                }
                let affected = folder_rewrite_plan(hosts, path, &target);
                if affected.is_empty() {
                    submit
                        .prefs
                        .push(FolderPrefsChange::AddEmptyFolder(target.clone()));
                } else {
                    submit.moves = affected;
                    submit.moves_label = Some(target.clone());
                }
                submit.move_settings = Some((path.clone(), target.clone()));
                submit.credentials = credentials_write(&target, draft, record);
                submit.toast = Some(format!("Folder renamed to “{target}”."));
            } else {
                submit.credentials =
                    credentials_write(&normalize_group_path(Some(path)), draft, record);
            }
            Some(submit)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hosts::test_support::ssh_host;
    use crate::model::HostMetadata;

    fn host(alias: &str, group: &str) -> ManagedHost {
        let mut entry = ssh_host(alias, Some(group));
        entry.metadata = Some(HostMetadata {
            profile_id: alias.into(),
            group: Some(group.into()),
            ..Default::default()
        });
        ManagedHost::ssh(entry)
    }

    fn record(path: &str) -> FolderSettingsRecord {
        FolderSettingsRecord {
            id: "f1".into(),
            path: path.into(),
            auth: FolderAuthSettings {
                user: Some("ops".into()),
                port: Some(2222),
                identity_files: Some(vec!["~/.ssh/lab".into()]),
                identities_only: Some(true),
                ..FolderAuthSettings::default()
            },
            has_password: true,
            login_sequence: None,
            created_at: String::new(),
            updated_at: String::new(),
        }
    }

    #[test]
    fn opens_an_existing_folder_with_its_style_and_credentials() {
        let mut prefs = FolderPrefs::default();
        prefs.set_folder_style(
            "folder:lab/fabric",
            Some(FolderStyle {
                color: Some("#a855f7".into()),
                icon: Some("lan".into()),
            }),
        );
        let state = FolderDialogState::Edit {
            path: "Lab/Fabric".into(),
        };
        let draft = FolderDraft::new(&state, &prefs, Some(&record("Lab/Fabric")));
        assert_eq!(
            (draft.name.as_str(), draft.parent.as_str()),
            ("Fabric", "Lab")
        );
        assert_eq!(draft.icon.as_deref(), Some("lan"));
        assert_eq!(
            (
                draft.auth_user.as_str(),
                draft.auth_port.as_str(),
                draft.auth_key.as_str()
            ),
            ("ops", "2222", "~/.ssh/lab")
        );
        assert_eq!(title(&state), "Edit “Fabric”");
        assert!(!renaming(&state, &draft));
    }

    #[test]
    fn renames_a_folder_and_moves_its_hosts() {
        let state = FolderDialogState::Edit {
            path: "Lab/Fabric".into(),
        };
        let mut draft = FolderDraft::new(&state, &FolderPrefs::default(), None);
        draft.name = "Spine / Leaf".into();
        assert_eq!(target_path(&state, &draft), "Lab/Spine Leaf");
        let hosts = vec![
            host("a", "Lab/Fabric"),
            host("b", "Lab"),
            host("c", "Lab/Fabric/Pod"),
        ];
        let submit = plan_submit(&state, &draft, &hosts, None, VaultState::default()).unwrap();
        assert_eq!(submit.moves.len(), 2);
        assert_eq!(submit.moves[1].group.as_deref(), Some("Lab/Spine Leaf/Pod"));
        assert_eq!(
            submit.toast.as_deref(),
            Some("Folder renamed to “Lab/Spine Leaf”.")
        );
        assert_eq!(
            submit.move_settings,
            Some(("Lab/Fabric".into(), "Lab/Spine Leaf".into()))
        );
        assert!(matches!(submit.prefs[1], FolderPrefsChange::Rename { .. }));
        assert_eq!(
            rename_note("Lab/Spine Leaf", 2, false).as_deref(),
            Some("Moves 2 hosts.")
        );
        let known = vec!["Lab".to_owned(), "Lab/Spine Leaf".to_owned()];
        assert!(merging(&state, &draft, &known));
    }

    #[test]
    fn refuses_a_folder_moved_inside_itself() {
        let state = FolderDialogState::Edit { path: "Lab".into() };
        let mut draft = FolderDraft::new(&state, &FolderPrefs::default(), None);
        draft.parent = "Lab/Fabric".into();
        assert_eq!(
            target_problem(&state, &draft),
            Some(FolderProblem::IntoDescendant)
        );
        assert_eq!(
            parent_helper_text(&state, &draft),
            (true, "A folder cannot be moved inside itself.".into())
        );
        assert_eq!(
            plan_submit(&state, &draft, &[], None, VaultState::default()),
            None
        );
    }

    #[test]
    fn writes_credentials_only_when_there_is_something_to_store() {
        let state = FolderDialogState::New {
            parent_path: Some("Lab".into()),
        };
        let mut draft = FolderDraft::new(&state, &FolderPrefs::default(), None);
        draft.name = "Edge".into();
        let submit = plan_submit(&state, &draft, &[], None, VaultState::default()).unwrap();
        assert_eq!(submit.credentials, None);
        assert_eq!(
            submit.prefs[0],
            FolderPrefsChange::AddEmptyFolder("Lab/Edge".into())
        );
        draft.auth_key = " ~/.ssh/edge ".into();
        draft.auth_port = "70000".into();
        assert!(port_invalid(&draft));
        assert_eq!(
            plan_submit(&state, &draft, &[], None, VaultState::default()),
            None
        );
        draft.auth_port = "22".into();
        let write = plan_submit(&state, &draft, &[], None, VaultState::default())
            .unwrap()
            .credentials
            .unwrap();
        assert_eq!(write.auth.identity_files, Some(vec!["~/.ssh/edge".into()]));
        assert_eq!(write.auth.identities_only, Some(true));
        assert_eq!(write.auth.port, Some(22));
        assert_eq!(write.password, None);
        assert_eq!(write.login_sequence, None);
    }

    #[test]
    fn asks_for_the_master_password_of_a_locked_vault() {
        let state = FolderDialogState::Edit { path: "Lab".into() };
        let mut draft = FolderDraft::new(&state, &FolderPrefs::default(), Some(&record("Lab")));
        let vault = VaultState {
            configured: true,
            locked: true,
        };
        draft.auth_password = "secret".into();
        assert!(password_needs_master(&draft, vault));
        assert!(submit_blocked(&state, &draft, vault));
        draft.master_password = "master".into();
        let write = plan_submit(&state, &draft, &[], Some(&record("Lab")), vault)
            .unwrap()
            .credentials
            .unwrap();
        assert_eq!(write.password, Some(Some("secret".into())));
        assert_eq!(write.master_password.as_deref(), Some("master"));
        draft.auth_password.clear();
        draft.remove_password = true;
        let write = credentials_write("Lab", &draft, Some(&record("Lab"))).unwrap();
        assert_eq!(write.password, Some(None));
        assert_eq!(
            password_helper_text(&draft, vault, true),
            "The saved password is removed when you save."
        );
    }

    #[test]
    fn moves_hosts_that_are_not_there_yet() {
        let hosts = vec![host("a", "Lab"), host("b", "Production")];
        let state = FolderDialogState::MoveHosts {
            host_keys: vec!["ssh:a".into(), "ssh:b".into()],
            host_name: None,
            current_path: String::new(),
        };
        let mut draft = FolderDraft::new(&state, &FolderPrefs::default(), None);
        draft.parent = "lab".into();
        let submit = plan_submit(&state, &draft, &hosts, None, VaultState::default()).unwrap();
        assert_eq!(submit.moves.len(), 1);
        assert_eq!(managed_host_key(&submit.moves[0].host), "ssh:b");
        assert_eq!(title(&state), "Move 2 hosts");
        assert_eq!(submit_label(&state), "Move");
    }
}
