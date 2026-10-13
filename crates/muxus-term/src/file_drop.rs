//! What dropping files onto a terminal does (port of the pure half of
//! `client/src/terminal/file-drop.ts`). SSH sessions upload them into the shell's directory; local
//! shells type their paths. Holding the insert modifier types the paths into an SSH session too.
//! Shift is the modifier everywhere: Ctrl, Alt, Option and Command already pick copy, move, link
//! or a menu in the file managers drags come from.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalDropIntent {
    /// No directory means the home directory, not yet resolved.
    Upload {
        directory: Option<String>,
        hint: Option<String>,
    },
    Insert {
        by_modifier: bool,
    },
    Unavailable {
        message: String,
        hint: Option<String>,
    },
    /// Not a drop target: no overlay, and the drop is refused.
    Refuse,
}

pub const INSERT_PATHS_HINT: &str = "Hold Shift to insert the local paths instead";

/// Session status as the tab reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropSessionStatus {
    Connecting,
    Connected,
    Interrupted,
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalDropContext {
    /// `ssh`, `local`, `telnet`, `serial`, `rdp`, `vnc`.
    pub profile_kind: String,
    pub status: DropSessionStatus,
    /// The live SSH connection; absent until the session is ready.
    pub conn_id: Option<String>,
    pub sftp_available: Option<bool>,
    /// Directory the shell last reported.
    pub cwd: Option<String>,
    /// The remote home directory, once known.
    pub home: Option<String>,
    /// The insert modifier is held.
    pub insert_modifier: bool,
    /// Local paths of dropped files can be read.
    pub can_insert_paths: bool,
}

pub fn terminal_drop_intent(context: &TerminalDropContext) -> TerminalDropIntent {
    let kind = context.profile_kind.as_str();
    if kind != "ssh" && kind != "local" {
        return TerminalDropIntent::Refuse;
    }
    if context.status == DropSessionStatus::Closed {
        return TerminalDropIntent::Unavailable {
            message: "The session has ended. Reconnect to drop files here.".into(),
            hint: None,
        };
    }
    if kind == "local" {
        return if context.can_insert_paths {
            TerminalDropIntent::Insert { by_modifier: false }
        } else {
            TerminalDropIntent::Unavailable {
                message: "File paths can only be inserted in the desktop app".into(),
                hint: None,
            }
        };
    }
    if context.insert_modifier && context.can_insert_paths {
        return TerminalDropIntent::Insert { by_modifier: true };
    }
    let hint = context
        .can_insert_paths
        .then(|| INSERT_PATHS_HINT.to_string());
    if context.sftp_available == Some(false) {
        return TerminalDropIntent::Unavailable {
            message: "SFTP is disabled for this host".into(),
            hint,
        };
    }
    if context.conn_id.is_none() {
        return TerminalDropIntent::Unavailable {
            message: "Waiting for the session to connect…".into(),
            hint,
        };
    }
    TerminalDropIntent::Upload {
        directory: context.cwd.clone().or_else(|| context.home.clone()),
        hint,
    }
}

/// The overlay shown while files are dragged over the terminal; `None` for a refused drop.
pub fn drop_overlay_text(intent: &TerminalDropIntent) -> Option<(String, Option<String>)> {
    match intent {
        TerminalDropIntent::Upload { directory, hint } => Some((
            format!(
                "Upload to {}",
                directory.as_deref().unwrap_or("the home folder")
            ),
            hint.clone(),
        )),
        TerminalDropIntent::Insert { by_modifier: true } => Some((
            "Insert the local paths".into(),
            Some("Release Shift to upload instead".into()),
        )),
        TerminalDropIntent::Insert { by_modifier: false } => {
            Some(("Insert the file paths".into(), None))
        }
        TerminalDropIntent::Unavailable { message, hint } => Some((message.clone(), hint.clone())),
        TerminalDropIntent::Refuse => None,
    }
}

/// The drop effect a drag offers: copy for an upload, link for inserted paths, none otherwise.
pub fn drop_effect_for(intent: &TerminalDropIntent) -> &'static str {
    match intent {
        TerminalDropIntent::Upload { .. } => "copy",
        TerminalDropIntent::Insert { .. } => "link",
        _ => "none",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ssh() -> TerminalDropContext {
        TerminalDropContext {
            profile_kind: "ssh".into(),
            status: DropSessionStatus::Connected,
            conn_id: Some("conn-1".into()),
            sftp_available: Some(true),
            cwd: None,
            home: None,
            insert_modifier: false,
            can_insert_paths: true,
        }
    }

    fn hint() -> Option<String> {
        Some(INSERT_PATHS_HINT.into())
    }

    #[test]
    fn uploads_into_the_shells_directory() {
        let context = TerminalDropContext {
            cwd: Some("/srv/app".into()),
            home: Some("/home/admin".into()),
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&context),
            TerminalDropIntent::Upload {
                directory: Some("/srv/app".into()),
                hint: hint()
            }
        );
    }

    #[test]
    fn uploads_into_the_home_directory_when_the_shell_reports_none() {
        let context = TerminalDropContext {
            home: Some("/home/admin".into()),
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&context),
            TerminalDropIntent::Upload {
                directory: Some("/home/admin".into()),
                hint: hint()
            }
        );
        assert_eq!(
            terminal_drop_intent(&ssh()),
            TerminalDropIntent::Upload {
                directory: None,
                hint: hint()
            }
        );
    }

    #[test]
    fn inserts_the_local_paths_while_the_modifier_is_held() {
        let context = TerminalDropContext {
            insert_modifier: true,
            cwd: Some("/srv".into()),
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&context),
            TerminalDropIntent::Insert { by_modifier: true }
        );
    }

    #[test]
    fn ignores_the_modifier_where_dropped_files_have_no_paths() {
        let context = TerminalDropContext {
            insert_modifier: true,
            can_insert_paths: false,
            cwd: Some("/srv".into()),
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&context),
            TerminalDropIntent::Upload {
                directory: Some("/srv".into()),
                hint: None
            }
        );
    }

    #[test]
    fn says_sftp_is_disabled_instead_of_offering_an_upload() {
        let disabled = TerminalDropContext {
            sftp_available: Some(false),
            cwd: Some("/srv".into()),
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&disabled),
            TerminalDropIntent::Unavailable {
                message: "SFTP is disabled for this host".into(),
                hint: hint()
            }
        );
        let no_paths = TerminalDropContext {
            sftp_available: Some(false),
            can_insert_paths: false,
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&no_paths),
            TerminalDropIntent::Unavailable {
                message: "SFTP is disabled for this host".into(),
                hint: None
            }
        );
        let modifier = TerminalDropContext {
            sftp_available: Some(false),
            insert_modifier: true,
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&modifier),
            TerminalDropIntent::Insert { by_modifier: true }
        );
    }

    #[test]
    fn waits_for_the_connection_and_not_for_an_ended_session() {
        let connecting = TerminalDropContext {
            status: DropSessionStatus::Connecting,
            conn_id: None,
            ..ssh()
        };
        assert!(matches!(
            terminal_drop_intent(&connecting),
            TerminalDropIntent::Unavailable { message, .. } if message == "Waiting for the session to connect…"
        ));
        let closed = TerminalDropContext {
            status: DropSessionStatus::Closed,
            conn_id: None,
            ..ssh()
        };
        assert!(matches!(
            terminal_drop_intent(&closed),
            TerminalDropIntent::Unavailable { message, .. } if message == "The session has ended. Reconnect to drop files here."
        ));
    }

    #[test]
    fn types_the_paths_into_a_local_shell() {
        let local = TerminalDropContext {
            profile_kind: "local".into(),
            conn_id: None,
            sftp_available: None,
            ..ssh()
        };
        assert_eq!(
            terminal_drop_intent(&local),
            TerminalDropIntent::Insert { by_modifier: false }
        );
        let modifier = TerminalDropContext {
            insert_modifier: true,
            ..local.clone()
        };
        assert_eq!(
            terminal_drop_intent(&modifier),
            TerminalDropIntent::Insert { by_modifier: false }
        );
        let browser = TerminalDropContext {
            can_insert_paths: false,
            ..local
        };
        assert_eq!(
            terminal_drop_intent(&browser),
            TerminalDropIntent::Unavailable {
                message: "File paths can only be inserted in the desktop app".into(),
                hint: None
            }
        );
    }

    #[test]
    fn offers_nothing_on_telnet_serial_and_remote_desktop_sessions() {
        for kind in ["telnet", "serial", "rdp", "vnc"] {
            let context = TerminalDropContext {
                profile_kind: kind.into(),
                insert_modifier: true,
                ..ssh()
            };
            assert_eq!(terminal_drop_intent(&context), TerminalDropIntent::Refuse);
        }
    }

    #[test]
    fn names_the_upload_target() {
        let upload = TerminalDropIntent::Upload {
            directory: Some("/home/admin/project".into()),
            hint: None,
        };
        assert_eq!(
            drop_overlay_text(&upload),
            Some(("Upload to /home/admin/project".into(), None))
        );
        let home = TerminalDropIntent::Upload {
            directory: None,
            hint: hint(),
        };
        assert_eq!(
            drop_overlay_text(&home),
            Some(("Upload to the home folder".into(), hint()))
        );
    }

    #[test]
    fn tells_inserting_by_the_modifier_apart() {
        assert_eq!(
            drop_overlay_text(&TerminalDropIntent::Insert { by_modifier: true }),
            Some((
                "Insert the local paths".into(),
                Some("Release Shift to upload instead".into())
            ))
        );
        assert_eq!(
            drop_overlay_text(&TerminalDropIntent::Insert { by_modifier: false }),
            Some(("Insert the file paths".into(), None))
        );
    }

    #[test]
    fn accepts_only_drops_it_acts_on() {
        assert_eq!(
            drop_effect_for(&TerminalDropIntent::Upload {
                directory: None,
                hint: None
            }),
            "copy"
        );
        assert_eq!(
            drop_effect_for(&TerminalDropIntent::Insert { by_modifier: true }),
            "link"
        );
        assert_eq!(
            drop_effect_for(&TerminalDropIntent::Unavailable {
                message: "x".into(),
                hint: None
            }),
            "none"
        );
        assert_eq!(drop_effect_for(&TerminalDropIntent::Refuse), "none");
    }
}
