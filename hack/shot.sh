#!/usr/bin/env bash
# Screenshot one GUI program in a throwaway headless GNOME Shell.
#
#   hack/shot.sh [options] <out.png> -- <command> [args...]
#
#   --size WxH     logical monitor size (default 1420x880, the docs screenshots)
#   --scale N      integer monitor scale (default 2, so the PNG is 2840x1760)
#   --wait SEC     seconds to let the program settle before the shot (default 6)
#   --home DIR     scratch HOME to use (kept afterwards); default: a temp dir
#                  that is deleted again
#   --env K=V      extra environment for the program (repeatable)
#   --name ID      suffix for the systemd unit and Wayland display (default: ui)
#   --dark         set the desktop color scheme to dark (prefer-dark) first
#   --step S       after the wait, drive the pointer (repeatable, in order):
#                  click:X,Y  rclick:X,Y  move:X,Y  wait:SECONDS  (logical px)
#                  key:CODE[+CODE...]  (evdev keycodes as one chord; key:42 taps
#                  Shift, which also gives the session a keyboard and so focus)
#
# Example:
#   hack/shot.sh --env MUXUS_THEME=dark /tmp/x.png -- target/debug/muxus
#
# What it does, and why:
# - Everything runs inside a transient systemd scope with a memory cap, and the
#   scope is always stopped at the end, so no child (gnome-shell, its
#   ibus-daemon, the program) can outlive the run. Never kill gnome-shell or
#   ibus-daemon by name: the real desktop session runs them too.
# - The shell gets a private D-Bus session, a scratch HOME/XDG_* and a short
#   XDG_RUNTIME_DIR (Wayland socket paths are capped at 108 bytes).
# - hack/shot_driver.py starts `gnome-shell --headless --wayland --no-x11` with
#   a virtual monitor of (size × scale) pixels, sets the monitor scale through
#   org.gnome.Mutter.DisplayConfig, owns org.gnome.Screenshot (the name the
#   shell's screenshot allowlist accepts), runs the program with
#   WAYLAND_DISPLAY pointing at the private shell and captures the whole
#   monitor with org.gnome.Shell.Screenshot.Screenshot.
# - The program's output goes to <out.png>.log, the shell's to <out.png>.log.shell.
#
# Programs that should fill the monitor (as the docs screenshots do) need to
# open fullscreen; muxus does that when MUXUS_WINDOW=fullscreen is set.
#
# Compare two PNGs with hack/compare.py.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
size=1420x880
scale=2
wait=6
home=""
name=ui
dark=0
steps=()
extra_env=()

while [[ $# -gt 0 ]]; do
  case "$1" in
    --size) size="$2"; shift 2 ;;
    --scale) scale="$2"; shift 2 ;;
    --wait) wait="$2"; shift 2 ;;
    --home) home="$2"; shift 2 ;;
    --env) extra_env+=("$2"); shift 2 ;;
    --name) name="$2"; shift 2 ;;
    --dark) dark=1; shift ;;
    --step) steps+=(--step "$2"); shift 2 ;;
    -h|--help) sed -n '2,34p' "$0"; exit 0 ;;
    --) shift; break ;;
    -*) echo "unknown option $1" >&2; exit 2 ;;
    *)
      if [[ -z "${out:-}" ]]; then out="$1"; shift; else break; fi
      ;;
  esac
done
[[ "${1:-}" == "--" ]] && shift
if [[ -z "${out:-}" || $# -eq 0 ]]; then
  echo "usage: hack/shot.sh [options] <out.png> -- <command> [args...]" >&2
  exit 2
fi

out="$(realpath -m "$out")"
mkdir -p "$(dirname "$out")"
cmd=("$@")
# Resolve the program now: the scope does not inherit the caller's cwd-relative paths.
if [[ "${cmd[0]}" == */* ]]; then cmd[0]="$(realpath -m "${cmd[0]}")"; fi

unit="muxusrs-${name}-$$"
runtime="$(mktemp -d /tmp/mx-XXXX)"
own_home=0
if [[ -z "$home" ]]; then
  home="$(mktemp -d "${TMPDIR:-/tmp}/mx-home-XXXXXX")"
  own_home=1
fi
mkdir -p "$home/.config" "$home/.local/share" "$home/.cache" "$home/.local/state"

cleanup() {
  systemctl --user stop "$unit.scope" >/dev/null 2>&1 || true
  # The document portal mounts a FUSE file system under the runtime dir; it
  # outlives the scope, so detach it before removing the directory.
  local mount
  while read -r _ mount _; do
    case "$mount" in
      "$runtime"/*) fusermount3 -u -z "$mount" 2>/dev/null || fusermount -u -z "$mount" 2>/dev/null || true ;;
    esac
  done </proc/mounts
  rm -rf "$runtime" 2>/dev/null || true
  if [[ $own_home -eq 1 ]]; then rm -rf "$home"; fi
}
trap cleanup EXIT INT TERM

status=0
: >"$out.log.shell"
systemd-run --user --scope --quiet --unit="$unit" \
  -p MemoryMax=6G -p MemorySwapMax=0 -- \
  env -u DBUS_SESSION_BUS_ADDRESS -u DISPLAY -u WAYLAND_DISPLAY -u ZDOTDIR \
    HOME="$home" \
    XDG_RUNTIME_DIR="$runtime" \
    XDG_CONFIG_HOME="$home/.config" \
    XDG_DATA_HOME="$home/.local/share" \
    XDG_CACHE_HOME="$home/.cache" \
    XDG_STATE_HOME="$home/.local/state" \
    NO_AT_BRIDGE=1 \
    "${extra_env[@]}" \
    dbus-run-session -- python3 -I "$here/shot_driver.py" \
      --size "$size" --scale "$scale" --wait "$wait" --display "mx-$name" --dark "$dark" "${steps[@]}" \
      --out "$out" --log "$out.log" -- "${cmd[@]}" 2>>"$out.log.shell" || status=$?

if [[ $status -eq 0 && -f "$out" ]]; then
  echo "$out"
else
  echo "shot failed (status $status); logs: $out.log, $out.log.shell" >&2
fi
exit $status
