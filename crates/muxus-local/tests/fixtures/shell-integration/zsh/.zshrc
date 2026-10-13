# Muxus shell integration. Your .zshrc runs first, unmodified.
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
