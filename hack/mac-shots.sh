#!/usr/bin/env bash
# Screenshots of the app and some of its examples on macOS, light and dark: what the
# macOS workflow (.github/workflows/macos.yml) runs on a GitHub runner.
#
#   hack/mac-shots.sh [--bin DIR] [--out DIR]
#
# --bin is where the programs are, the examples in its examples/ (default target/debug:
# `cargo build -p muxus-app --bin muxus --example settings --example sftp …`, see the list
# at the bottom). Each shot starts the program with a scratch data folder, waits for
# its window, lets it settle, captures the window alone (screencapture -l) and quits it.
# The program's output lands next to each PNG as <name>.log; when no window shows up the
# whole screen is captured as <name>-screen.png instead. Capturing needs the Screen
# Recording permission, which the GitHub runners grant to bash.
#
# Exits 1 when a shot failed (the others are still taken).

set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
bin="$root/target/debug"
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --bin) bin="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,15p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
bin="$(cd "$bin" && pwd)"
if [[ -z "$out" ]]; then out="$(mktemp -d "${TMPDIR:-/tmp}/muxus-shots-XXXXXX")"; fi
mkdir -p "$out"
out="$(cd "$out" && pwd)"
scratch="$(mktemp -d "${TMPDIR:-/tmp}/muxus-mac-shots-XXXXXX")"
trap 'rm -rf "$scratch"' EXIT

# The window of a process: the largest on-screen, normal-level window it owns.
cat >"$scratch/window-id.swift" <<'SWIFT'
import CoreGraphics
import Foundation

guard CommandLine.arguments.count == 2, let pid = Int(CommandLine.arguments[1]) else { exit(2) }
let windows = CGWindowListCopyWindowInfo([.optionOnScreenOnly, .excludeDesktopElements], kCGNullWindowID)
    as? [[String: Any]] ?? []
func area(_ window: [String: Any]) -> Double {
    let bounds = window[kCGWindowBounds as String] as? [String: Double] ?? [:]
    return (bounds["Width"] ?? 0) * (bounds["Height"] ?? 0)
}
let mine = windows.filter {
    ($0[kCGWindowOwnerPID as String] as? Int) == pid && ($0[kCGWindowLayer as String] as? Int) == 0
}
guard let window = mine.max(by: { area($0) < area($1) }), area(window) > 0,
    let number = window[kCGWindowNumber as String] as? Int
else { exit(1) }
print(number)
SWIFT
swiftc -O -o "$scratch/window-id" "$scratch/window-id.swift" || exit 2

failed=()

# shot <name> <program> <settle seconds> [VAR=value...]: one light and one dark capture.
shot() {
  local name="$1" program="$2" settle="$3"
  shift 3
  local theme
  for theme in light dark; do
    shot_one "$name-$theme" "$program" "$settle" MUXUS_THEME="$theme" "$@"
  done
}

shot_one() {
  local name="$1" program="$2" settle="$3"
  shift 3
  local data="$scratch/$name" png="$out/$name.png" log="$out/$name.log"
  local path="$bin/$program"
  [[ -x "$path" ]] || path="$bin/examples/$program"
  mkdir -p "$data"
  env MUXUS_DATA_DIR="$data" MUXUS_WINDOW=1420x880 "$@" "$path" >"$log" 2>&1 &
  local pid=$! id="" waited=0
  while [[ -z "$id" && $waited -lt 60 ]] && kill -0 "$pid" 2>/dev/null; do
    sleep 1
    waited=$((waited + 1))
    id="$("$scratch/window-id" "$pid" 2>/dev/null)" || id=""
  done
  if [[ -n "$id" ]]; then
    sleep "$settle"
    # The window may have been replaced while it settled (a fullscreen or restored one).
    id="$("$scratch/window-id" "$pid" 2>/dev/null)" || id=""
  fi
  if [[ -n "$id" ]] && screencapture -x -o -l "$id" "$png" && [[ -s "$png" ]]; then
    echo "$name: ok"
  else
    if kill -0 "$pid" 2>/dev/null; then
      echo "$name: no window after ${waited}s" | tee -a "$log"
    else
      wait "$pid"
      echo "$name: exited with status $? before a window showed" | tee -a "$log"
    fi
    screencapture -x "$out/$name-screen.png" || true
    failed+=("$name")
  fi
  kill "$pid" 2>/dev/null
  # Give it a moment to quit, then make sure it has.
  local tries=0
  while kill -0 "$pid" 2>/dev/null && [[ $tries -lt 10 ]]; do sleep 0.5; tries=$((tries + 1)); done
  kill -9 "$pid" 2>/dev/null
  wait "$pid" 2>/dev/null
}

# The main window with the documentation's scenes (crates/muxus-app/src/ui/scenes.rs).
shot app-empty muxus 3 MUXUS_SCENE=empty
shot app-overview muxus 7 MUXUS_SCENE=overview
shot app-panes muxus 11 MUXUS_SCENE=panes
shot app-local muxus 6 MUXUS_SCENE=local
shot app-localsplit muxus 7 MUXUS_SCENE=localsplit
# The examples, each a part of the app on a sandboxed backend.
shot settings settings 4
shot settings-terminal settings 4 MUXUS_SETTINGS_SECTION=terminal
shot host-editor host_editor 4
shot launcher launcher 8 TZ=UTC
shot workspaces workspaces 4 TZ=UTC
shot sftp sftp 8 MUXUS_SCENE=sftp
shot remote-editor sftp 8 MUXUS_SCENE=remote-editor
shot remote-desktop remote_desktop 8

echo "shots in $out"
if [[ ${#failed[@]} -gt 0 ]]; then
  echo "failed: ${failed[*]}" >&2
  exit 1
fi
