//! The remote shell-integration scripts, byte for byte as the TypeScript build writes them.
//! Their content hash names the remote install directory, so both apps share one install.

/// Prints the login shell, ZDOTDIR and HOME between markers.
pub const SHELL_PROBE: &str = r##"command printf '\n__MUXUS_SHELL__=%s\n__MUXUS_ZDOTDIR__=%s\n__MUXUS_HOME__=%s\n' "${SHELL-}" "${ZDOTDIR-}" "${HOME-}""##;

/// The zsh bootstrap, the cwd prompt hook and the zsh rc shim are the local integration's
/// (`local/shell-integration.ts`), shared through `muxus_core`.
pub use muxus_core::shell_integration::{SHELL_CWD_REPORT, ZSHENV, ZSHRC};

/// Preserves login-zsh startup while ZDOTDIR points at the Muxus shim.
pub const ZPROFILE: &str = r##"# Preserve login-zsh startup while ZDOTDIR points at the Muxus shim.
__muxus_shim="$ZDOTDIR"
if [[ -n "$MUXUS_USER_ZDOTDIR" ]]; then ZDOTDIR="$MUXUS_USER_ZDOTDIR"; else builtin unset ZDOTDIR; fi
if [[ -f "${ZDOTDIR:-$HOME}/.zprofile" ]]; then builtin source "${ZDOTDIR:-$HOME}/.zprofile"; fi
if [[ -n "$ZDOTDIR" ]]; then MUXUS_USER_ZDOTDIR="$ZDOTDIR"; else builtin unset MUXUS_USER_ZDOTDIR; fi
ZDOTDIR="$__muxus_shim"
builtin unset __muxus_shim
"##;

/// Runs last in bash and zsh startup, so the user's own rc files cannot undo it and nothing is typed into the shell (where it would echo and land in history).
pub const RESTORE_CWD: &str = r##"
if [[ -n "${MUXUS_RESTORE_CWD-}" ]]; then
  builtin cd -- "$MUXUS_RESTORE_CWD" 2>/dev/null ||
    builtin printf 'Muxus: could not restore the previous working directory.\n'
fi
builtin unset MUXUS_RESTORE_CWD
"##;

/// The remote .zshrc: the shared zsh shim plus the working-directory restore.
pub const REMOTE_ZSHRC: &str = r##"# Muxus shell integration. Your .zshrc runs first, unmodified.
if [[ -n "$MUXUS_USER_ZDOTDIR" ]]; then ZDOTDIR="$MUXUS_USER_ZDOTDIR"; else builtin unset ZDOTDIR; fi
builtin unset MUXUS_USER_ZDOTDIR
if [[ -f "${ZDOTDIR:-$HOME}/.zshrc" ]]; then builtin source "${ZDOTDIR:-$HOME}/.zshrc"; fi

if [[ -o interactive && -z "$__muxus_integrated" ]]; then
  builtin typeset -g __muxus_integrated=1
  builtin autoload -Uz add-zsh-hook
  __muxus_report_cwd() {
    local __muxus_cwd="$PWD"
    __muxus_cwd="${__muxus_cwd//\\/\\\\}"
    __muxus_cwd="${__muxus_cwd//;/\\x3b}"
    builtin printf '\e]133;P;Cwd=%s\a' "$__muxus_cwd"
  }

  __muxus_precmd() {
    local __muxus_status=$?
    builtin printf '\e]133;D;%s\a' "$__muxus_status"
    __muxus_report_cwd
    builtin printf '\e]133;A\a'
  }
  __muxus_preexec() {
    builtin printf '\e]133;C\a'
  }
  add-zsh-hook precmd __muxus_precmd
  add-zsh-hook preexec __muxus_preexec
fi

if [[ -n "${MUXUS_RESTORE_CWD-}" ]]; then
  builtin cd -- "$MUXUS_RESTORE_CWD" 2>/dev/null ||
    builtin printf 'Muxus: could not restore the previous working directory.\n'
fi
builtin unset MUXUS_RESTORE_CWD
"##;

/// The remote bash rc file: reproduces login startup first, then adds the prompt marks.
pub const BASH_INIT: &str = r##"# Muxus SSH shell integration. Reproduce login startup first.
if [[ -f /etc/profile ]]; then . /etc/profile; fi
if [[ -f "$HOME/.bash_profile" ]]; then
  . "$HOME/.bash_profile"
elif [[ -f "$HOME/.bash_login" ]]; then
  . "$HOME/.bash_login"
elif [[ -f "$HOME/.profile" ]]; then
  . "$HOME/.profile"
fi

if [[ $- == *i* && -z "$__muxus_integrated" ]]; then
  __muxus_integrated=1
  __muxus_report_cwd() {
    local __muxus_cwd="$PWD"
    __muxus_cwd="${__muxus_cwd//\\/\\\\}"
    __muxus_cwd="${__muxus_cwd//;/\\x3b}"
    builtin printf '\e]133;P;Cwd=%s\a' "$__muxus_cwd"
  }

  __muxus_prompt_mark() {
    local __muxus_status=$?
    printf '\e]133;D;%s\a' "$__muxus_status"
    __muxus_report_cwd
    printf '\e]133;A\a'
  }
  PS0="\[\e]133;C\a\]${PS0-}"
  PROMPT_COMMAND="__muxus_prompt_mark${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi

if [[ -n "${MUXUS_RESTORE_CWD-}" ]]; then
  builtin cd -- "$MUXUS_RESTORE_CWD" 2>/dev/null ||
    builtin printf 'Muxus: could not restore the previous working directory.\n'
fi
builtin unset MUXUS_RESTORE_CWD
"##;

/// The first 12 hex digits of the scripts' SHA-256, as computed by the TypeScript build.
pub const TYPESCRIPT_INTEGRATION_VERSION: &str = "5fd060055802";
