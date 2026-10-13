#!/usr/bin/env bash
# The app's screenshot set, light and dark, compared with the docs screenshots.
#
#   hack/app-shots.sh [--binary target/debug/muxus] [--out DIR]
#
# Fullscreen scenes (1420x880 at 2x, as the docs screenshots): overview and
# panes (compared with ../muxus/docs/assets/screenshots/<scene>[-dark].png),
# local and localsplit (local shells, captured only). Unmaximized window on a
# 1700x1100 monitor, light and dark: checked with hack/window_edge.py for a
# light outline around the window (the frame must shade into the desktop the
# way Electron's does). Each run takes its own headless GNOME Shell, one at a
# time (hack/shot.sh).

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
binary="$root/target/debug/muxus"
out=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --binary) binary="$(realpath -m "$2")"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    -h|--help) sed -n '2,13p' "$0"; exit 0 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
[[ -x "$binary" ]] || { echo "no binary at $binary (cargo build -p muxus-app first)" >&2; exit 2; }
if [[ -z "$out" ]]; then out="$(mktemp -d "${TMPDIR:-/tmp}/muxus-shots-XXXXXX")"; fi
mkdir -p "$out"
out="$(realpath "$out")"
refs="$root/../muxus/docs/assets/screenshots"

for scene in overview panes local localsplit; do
  for theme in light dark; do
    suffix=""; [[ $theme == dark ]] && suffix="-dark"
    wait=7; [[ $scene == panes ]] && wait=11
    timeout 200 "$here/shot.sh" --name app --wait "$wait" --step key:42 --step wait:0.5 \
      --env MUXUS_WINDOW=fullscreen --env MUXUS_SCENE=$scene --env MUXUS_THEME=$theme \
      "$out/$scene$suffix.png" -- "$binary" >/dev/null
    if [[ -f "$refs/$scene$suffix.png" ]]; then
      echo "$scene$suffix: $(python3 "$here/compare.py" "$out/$scene$suffix.png" "$refs/$scene$suffix.png" "$out/$scene$suffix-cmp" | grep differing)"
    else
      echo "$scene$suffix: captured"
    fi
  done
done

for theme in light dark; do
  dark=(); [[ $theme == dark ]] && dark=(--dark)
  timeout 200 "$here/shot.sh" --name app --size 1700x1100 --wait 6 "${dark[@]}" \
    --step key:42 --step wait:0.5 --env MUXUS_SCENE=local --env MUXUS_THEME=$theme \
    "$out/unmaximized-$theme.png" -- "$binary" >/dev/null
done
python3 "$here/window_edge.py" "$out/unmaximized-light.png" "$out/unmaximized-dark.png"
echo "shots in $out"
