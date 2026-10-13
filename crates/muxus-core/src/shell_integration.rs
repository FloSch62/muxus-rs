//! Shell integration shims shared by local terminals and SSH sessions (port of
//! `server/src/local/shell-integration.ts`): rc files that report the command lifecycle via
//! OSC 133 (FinalTerm), `D;<exit code>` when a command finishes and `C` when one starts, plus
//! the working directory as an OSC 133 `P;Cwd=` property. The terminal turns them into
//! failed-command marks and follows the directory. The shims source the user's own startup files
//! first, so prompts and plugins are untouched.
//!
//! The contents are byte for byte those of the TypeScript build: the remote install directory is
//! named after their hash, so both builds share one install on a host.

/// The zsh `.zshenv` shim. Sources the user's own `.zshenv` unmodified, then
/// returns to the shim so `.zshrc` can hook in.
pub const ZSHENV: &str = r##"# Muxus shell integration bootstrap. Sources your own .zshenv unmodified,
# then returns to the shim so .zshrc can hook in.
__muxus_shim="$ZDOTDIR"
if [[ -n "$MUXUS_USER_ZDOTDIR" ]]; then ZDOTDIR="$MUXUS_USER_ZDOTDIR"; else builtin unset ZDOTDIR; fi
if [[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]]; then builtin source "${ZDOTDIR:-$HOME}/.zshenv"; fi
if [[ -n "$ZDOTDIR" ]]; then MUXUS_USER_ZDOTDIR="$ZDOTDIR"; else builtin unset MUXUS_USER_ZDOTDIR; fi
ZDOTDIR="$__muxus_shim"
builtin unset __muxus_shim
"##;

/// Expands to the prompt hook shared by the bash and zsh shims, so it can be
/// spliced into both with `concat!`.
macro_rules! shell_cwd_report {
    () => {
        r##"  __muxus_report_cwd() {
    local __muxus_cwd="$PWD"
    __muxus_cwd="${__muxus_cwd//\\/\\\\}"
    __muxus_cwd="${__muxus_cwd//;/\\x3b}"
    builtin printf '\e]133;P;Cwd=%s\a' "$__muxus_cwd"
  }
"##
    };
}

/// Prompt hook shared by local and remote bash/zsh integrations. The value
/// follows VS Code's OSC 133/633 property escaping for backslashes and the
/// protocol's semicolon separator.
pub const SHELL_CWD_REPORT: &str = shell_cwd_report!();

/// The zsh `.zshrc` shim. The user's `.zshrc` runs first, unmodified.
pub const ZSHRC: &str = concat!(
    r##"# Muxus shell integration. Your .zshrc runs first, unmodified.
if [[ -n "$MUXUS_USER_ZDOTDIR" ]]; then ZDOTDIR="$MUXUS_USER_ZDOTDIR"; else builtin unset ZDOTDIR; fi
builtin unset MUXUS_USER_ZDOTDIR
if [[ -f "${ZDOTDIR:-$HOME}/.zshrc" ]]; then builtin source "${ZDOTDIR:-$HOME}/.zshrc"; fi

if [[ -o interactive && -z "$__muxus_integrated" ]]; then
  builtin typeset -g __muxus_integrated=1
  builtin autoload -Uz add-zsh-hook
"##,
    shell_cwd_report!(),
    r##"
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
"##
);

/// The bash `--init-file` of local terminals. The user's normal startup files run first,
/// unmodified. (SSH sessions install their own bash shim.)
pub const LOCAL_BASH_INIT: &str = concat!(
    r##"# Muxus shell integration. Your normal startup files run first, unmodified.
if [[ -f /etc/bash.bashrc ]]; then . /etc/bash.bashrc; fi
if [[ -f "$HOME/.bashrc" ]]; then . "$HOME/.bashrc"; fi

if [[ $- == *i* && -z "$__muxus_integrated" ]]; then
  __muxus_integrated=1
"##,
    shell_cwd_report!(),
    r##"
  __muxus_prompt_mark() {
    local __muxus_status=$?
    printf '\e]133;D;%s\a' "$__muxus_status"
    __muxus_report_cwd
    printf '\e]133;A\a'
  }
  # PS0 expands once per executed command (never for an empty prompt), which
  # makes it a preexec without the DEBUG-trap fragility.
  PS0="\[\e]133;C\a\]${PS0-}"
  PROMPT_COMMAND="__muxus_prompt_mark${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
"##
);
