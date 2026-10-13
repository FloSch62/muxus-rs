# Muxus shell integration. Your normal startup files run first, unmodified.
if [[ -f /etc/bash.bashrc ]]; then . /etc/bash.bashrc; fi
if [[ -f "$HOME/.bashrc" ]]; then . "$HOME/.bashrc"; fi

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
  # PS0 expands once per executed command (never for an empty prompt), which
  # makes it a preexec without the DEBUG-trap fragility.
  PS0="\[\e]133;C\a\]${PS0-}"
  PROMPT_COMMAND="__muxus_prompt_mark${PROMPT_COMMAND:+;$PROMPT_COMMAND}"
fi
