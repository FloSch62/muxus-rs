//! How a terminal tab presents a lost or ended session and when it redials:
//! `muxus_session::recovery` (the port of `client/src/connection-recovery.ts`
//! the backend shares with the window), plus the tab state's reattach mode.

pub use muxus_session::recovery::*;

use crate::state::tabs::ReattachMode as TabReattachMode;

/// The shell input that reattaches a tab's multiplexer after a reconnect.
pub fn reattach_command_for(mode: TabReattachMode) -> &'static str {
    reattach_command(match mode {
        TabReattachMode::Tmux => ReattachMode::Tmux,
        TabReattachMode::Screen => ReattachMode::Screen,
    })
}

/// Whether the bytes typed into an ended session are the diagnose key.
pub fn is_diagnose_input(data: &[u8]) -> bool {
    std::str::from_utf8(data).is_ok_and(is_diagnose_key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_the_tab_reattach_mode() {
        assert!(reattach_command_for(TabReattachMode::Tmux).contains("tmux attach-session"));
        assert!(reattach_command_for(TabReattachMode::Screen).contains("screen -xRR"));
        assert!(is_diagnose_input(b"D"));
        assert!(!is_diagnose_input(b"\xff"));
    }
}
