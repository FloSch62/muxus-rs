# Muxus shell integration bootstrap. Sources your own .zshenv unmodified,
# then returns to the shim so .zshrc can hook in.
__muxus_shim="$ZDOTDIR"
if [[ -n "$MUXUS_USER_ZDOTDIR" ]]; then ZDOTDIR="$MUXUS_USER_ZDOTDIR"; else builtin unset ZDOTDIR; fi
if [[ -f "${ZDOTDIR:-$HOME}/.zshenv" ]]; then builtin source "${ZDOTDIR:-$HOME}/.zshenv"; fi
if [[ -n "$ZDOTDIR" ]]; then MUXUS_USER_ZDOTDIR="$ZDOTDIR"; else builtin unset MUXUS_USER_ZDOTDIR; fi
ZDOTDIR="$__muxus_shim"
builtin unset __muxus_shim
