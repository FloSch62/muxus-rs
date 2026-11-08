#!/usr/bin/env bash
# Compile the Windows and macOS code from Linux, as far as that goes without their C
# toolchains (no mingw gcc, no clang for Apple targets).
#
#   hack/cross-check.sh
#
# Needs the targets: rustup target add x86_64-pc-windows-gnu x86_64-apple-darwin
#
# 1. `cargo clippy --all-targets -D warnings` for both targets on every crate without a C
#    build step (the list below). The others build C: muxus-store (bundled SQLite, zstd),
#    muxus-ssh (aws-lc), muxus-rdp (libz), muxus-mgmt (ring), muxus-ui (gpui's shaders on
#    Windows, tree-sitter on macOS), and through them muxus-session, muxus-term-view and
#    muxus-app.
# 2. hack/cross-check: the platform modules of those crates (OS credential store, history
#    disk space, single instance, local host stats, connection diagnostics, installer
#    hand-off) compiled on their own, the same way.
#
# Nothing here runs the code; it only proves it compiles and passes clippy.

set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(dirname "$here")"
export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$root/target-cross}"

crates=(
  muxus-core muxus-os muxus-serial muxus-local muxus-telnet muxus-term muxus-import
  muxus-vnc muxus-webview
)
packages=()
for crate in "${crates[@]}"; do
  packages+=(-p "$crate")
done

# The harness resolves to the app's versions; its copy of the lock goes again afterwards.
cp "$root/Cargo.lock" "$here/cross-check/Cargo.lock"
trap 'rm -f "$here/cross-check/Cargo.lock"' EXIT

for target in x86_64-pc-windows-gnu x86_64-apple-darwin; do
  echo "== $target: ${crates[*]}"
  (cd "$root" && cargo clippy "${packages[@]}" --all-targets --target "$target" -- -D warnings)
  echo "== $target: hack/cross-check"
  (cd "$here/cross-check" && cargo clippy --all-targets --target "$target" -- -D warnings)
done
