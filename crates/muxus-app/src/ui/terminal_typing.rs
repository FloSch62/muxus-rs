//! Typing into terminal tabs: saved text at the host's paste pace, the host's keys for pastes
//! and mirrored keystrokes, the note a paste that stopped by itself leaves, and local text
//! files. Ports `typeIntoSession`, `sendKeyboardInput` and the paste-progress handling of
//! `client/src/components/TerminalViewImpl.tsx`, `state/paste-progress.ts`
//! (`markPasteStopNoted`, `pasteStoppedNotice`), `broadcastTerminalTyping` of
//! `state/multi-exec.ts` and `terminal/send-text-file.ts`.

use std::path::PathBuf;

use muxus_app::features::launcher::catalog::BreakState;
use muxus_app::session::LinkInput;
use muxus_app::session::recovery::terminal_notice;
use muxus_app::state::paste_progress::PasteProgress;
use muxus_app::state::tabs::TabPatch;
use muxus_core::input_modes::{TerminalInputModes, host_terminal_input_modes};
use muxus_core::protocol::{PasteProgressState, TerminalClientMessage};
use muxus_term_view::muxus_term::send_text_file::{TextFileOutcome, text_file_outcome};
use muxus_term_view::muxus_term::typed_input::{TypedInput, typed_input};
use muxus_ui::gpui_kit::{AppContext, Context, PathPromptOptions, Window};
use muxus_ui::theme::Status;

use crate::ui::shell::AppShell;

/// The note a terminal shows when a paste waiting for the prompt stopped by itself.
pub fn paste_stopped_notice(message: Option<&str>) -> String {
    let reason = terminal_notice(message.unwrap_or("a line was rejected"));
    let reason = reason.strip_suffix('.').unwrap_or(&reason);
    format!("\x1b[33m[paste stopped: {reason}]\x1b[0m\r\n")
}

impl AppShell {
    /// What the tab's host's Backspace and Enter keys send, its implicit CR and local echo,
    /// read when they are used so a change to the host reaches open tabs.
    pub fn input_modes(&self, tab_id: &str) -> TerminalInputModes {
        let metadata = self
            .tabs
            .tab(tab_id)
            .and_then(|tab| tab.profile.as_ref())
            .and_then(|profile| self.host_metadata_for(profile));
        host_terminal_input_modes(metadata.as_ref())
    }

    /// A paced paste went out, or one is running: saved text queues behind it.
    fn paste_pending(&self, tab_id: &str) -> bool {
        self.terminals
            .get(tab_id)
            .and_then(|terminal| terminal.session.as_ref())
            .is_some_and(|session| session.paced_paste_pending || session.paste_progress.is_some())
    }

    /// The paced paste running in the tab's session, and how far it got.
    pub fn paste_progress(&self, tab_id: &str) -> Option<PasteProgress> {
        self.terminals
            .get(tab_id)
            .and_then(|terminal| terminal.session.as_ref())
            .and_then(|session| session.paste_progress)
    }

    /// `cancelPaste`: stop a paced paste. Input is mirrored under multi-execution, so stopping
    /// it in one mirrored terminal stops it in all of them.
    pub fn cancel_paste(&mut self, tab_id: &str, cx: &mut Context<Self>) {
        let mirrored = self.multi_exec.read(cx).state().mirrored_tab_ids(tab_id);
        let targets = if mirrored.is_empty() {
            vec![tab_id.to_string()]
        } else {
            mirrored
        };
        for target in targets {
            self.send_to_session(
                &target,
                LinkInput::message(TerminalClientMessage::PasteCancel),
                cx,
            );
        }
    }

    /// Note that a paced paste went out to the tab's session.
    pub(super) fn mark_paced_paste(&mut self, tab_id: &str) {
        if let Some(session) = self
            .terminals
            .get_mut(tab_id)
            .and_then(|terminal| terminal.session.as_mut())
        {
            session.paced_paste_pending = true;
        }
    }

    /// `typeText`: type saved text as keyboard input, never bracketed, at the host's paste
    /// pace and with its Enter key. False when the session takes no input.
    pub fn type_text(&mut self, tab_id: &str, text: &str, cx: &mut Context<Self>) -> bool {
        let Some(view) = self.terminal_of(tab_id) else {
            return false;
        };
        let modes = self.input_modes(tab_id);
        let input = typed_input(
            text,
            &self.paste_pacing(tab_id),
            self.paste_pending(tab_id),
            modes.enter_key,
        );
        match input {
            TypedInput::Direct(data) => {
                if data.is_empty() {
                    return true;
                }
                if !view.update(cx, |view, _| view.send_input(data.into_bytes())) {
                    return false;
                }
            }
            TypedInput::Paced(message) => {
                if !self.send_to_session(tab_id, LinkInput::message(message), cx) {
                    return false;
                }
                self.mark_paced_paste(tab_id);
            }
        }
        view.update(cx, |view, cx| view.echo_input(text, cx));
        true
    }

    /// `broadcastTerminalTyping`: type saved text into every other mirrored terminal, each at
    /// its own host's pace.
    pub fn mirror_typing(
        &mut self,
        source_tab_id: &str,
        text: &str,
        cx: &mut Context<Self>,
    ) -> usize {
        let targets = self
            .multi_exec
            .read(cx)
            .state()
            .mirror_tab_ids(source_tab_id);
        targets
            .iter()
            .filter(|target| self.type_text(target, text, cx))
            .count()
    }

    /// A `paste-progress` message: the progress of a running paste (shown on the tab and over
    /// its terminal), and the note of one that stopped, written once per stop even when no
    /// window was attached at the time (a returning one is sent it again).
    pub(super) fn on_paste_progress(
        &mut self,
        tab_id: &str,
        state: PasteProgressState,
        progress: Option<PasteProgress>,
        message: Option<String>,
        stop_id: Option<u64>,
        cx: &mut Context<Self>,
    ) {
        if let Some(session) = self
            .terminals
            .get_mut(tab_id)
            .and_then(|terminal| terminal.session.as_mut())
        {
            if session.paste_progress != progress {
                session.paste_progress = progress;
                cx.notify();
            }
            if state != PasteProgressState::Running {
                session.paced_paste_pending = false;
            }
        }
        if state == PasteProgressState::Stopped && self.mark_paste_stop_noted(tab_id, stop_id) {
            let notice = paste_stopped_notice(message.as_deref());
            self.write_notice_on_own_line(tab_id, &notice, cx);
        }
    }

    /// `markPasteStopNoted`: false when the tab noted this stop already.
    fn mark_paste_stop_noted(&mut self, tab_id: &str, stop_id: Option<u64>) -> bool {
        let noted = self.tabs.tab(tab_id).and_then(|tab| tab.paste_stop_noted);
        if stop_id.is_some() && noted == stop_id {
            return false;
        }
        self.tabs.update(
            tab_id,
            TabPatch {
                paste_stop_noted: Some(stop_id),
                ..Default::default()
            },
        );
        true
    }

    /// `canSendTextFile`: the serial, Telnet and SSH sessions that take BREAK, once attached.
    pub fn can_send_text_file(&self, tab_id: &str) -> bool {
        BreakState::of(self.tabs.tab(tab_id)) == BreakState::Ready
    }

    /// `requestSendTextFile`: ask for a local text file to type into the tab; false when the
    /// tab takes none.
    pub fn request_send_text_file(
        &mut self,
        tab_id: &str,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        if !self.can_send_text_file(tab_id) {
            return false;
        }
        let chosen = muxus_app::util::file_dialog::pick_paths(
            PathPromptOptions {
                files: true,
                directories: false,
                multiple: false,
                prompt: None,
            },
            cx,
        );
        let tab_id = tab_id.to_string();
        cx.spawn_in(window, async move |this, cx| {
            let Some(path) = chosen.await.and_then(|paths| paths.into_iter().next()) else {
                return;
            };
            let outcome = cx
                .background_spawn(async move { read_text_file(path) })
                .await;
            let _ = this.update_in(cx, |shell, window, cx| {
                shell.send_text_file(&tab_id, outcome, window, cx)
            });
        })
        .detach();
        true
    }

    /// `sendTextFile`: type a file into the tab and the sessions mirroring it, each at its own
    /// host's paste pace, as a command button does.
    fn send_text_file(
        &mut self,
        tab_id: &str,
        outcome: TextFileOutcome,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let text = match outcome {
            TextFileOutcome::Problem(problem) => {
                self.show_toast(Status::Error, problem, cx);
                return;
            }
            TextFileOutcome::Empty(note) => {
                self.show_toast(Status::Info, note, cx);
                return;
            }
            TextFileOutcome::Type(text) => text,
        };
        if !self.can_send_text_file(tab_id) || !self.type_text(tab_id, &text, cx) {
            self.show_toast(Status::Error, "The session is no longer connected.", cx);
            return;
        }
        self.mirror_typing(tab_id, &text, cx);
        self.focus_terminal(tab_id, window, cx);
    }
}

/// A chosen file, checked and read off the UI thread.
fn read_text_file(path: PathBuf) -> TextFileOutcome {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    let size = match std::fs::metadata(&path) {
        Ok(metadata) => metadata.len(),
        Err(error) => return TextFileOutcome::Problem(format!("Could not read “{name}”: {error}")),
    };
    let mut failed = None;
    let outcome = text_file_outcome(&name, size, || match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            failed = Some(error.to_string());
            Vec::new()
        }
    });
    match failed {
        Some(error) => TextFileOutcome::Problem(format!("Could not read “{name}”: {error}")),
        None => outcome,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_a_stopped_paste_in_yellow_on_a_line_of_its_own() {
        assert_eq!(
            paste_stopped_notice(Some("Line 2 of 3 (“bogus”) was answered with “% Invalid”")),
            "\x1b[33m[paste stopped: Line 2 of 3 (“bogus”) was answered with “% Invalid”]\x1b[0m\r\n"
        );
        assert_eq!(
            paste_stopped_notice(Some(
                "Line 1 of 2: the prompt pattern took too long to match."
            )),
            "\x1b[33m[paste stopped: Line 1 of 2: the prompt pattern took too long to match]\x1b[0m\r\n"
        );
        assert_eq!(
            paste_stopped_notice(None),
            "\x1b[33m[paste stopped: a line was rejected]\x1b[0m\r\n"
        );
    }
}
