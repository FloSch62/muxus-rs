# Shared by build-deb.sh and build-appimage.sh. Source it; do not run it.
#
# Inputs (environment):
#   MUXUS_BINARY       the release binary (default: $CARGO_TARGET_DIR or target, release/muxus)
#   MUXUS_VERSION      the package version (default: the workspace version)
#   MUXUS_PACKAGE_DIR  where packages go (default: packages/ next to the binary)

set -euo pipefail
umask 022

PACKAGING="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$PACKAGING/.." && pwd)"
export PATH="$HOME/.cargo/bin:$PATH"

VERSION="${MUXUS_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)}"
BINARY="${MUXUS_BINARY:-${CARGO_TARGET_DIR:-$ROOT/target}/release/muxus}"
OUT="${MUXUS_PACKAGE_DIR:-$(dirname "$BINARY")/packages}"
ICON_SIZES=(16 24 32 48 64 128 256 512 1024)

die() {
    echo "error: $*" >&2
    exit 1
}

[ -n "$VERSION" ] || die "could not read the version from $ROOT/Cargo.toml"
[ -x "$BINARY" ] || die "no release binary at $BINARY (cargo build --release -p muxus-app)"
mkdir -p "$OUT"

# Node-style CPU name for file names (matches the release feeds), Debian architecture.
case "$(uname -m)" in
    x86_64) ARCH_FILE=x86_64; DEB_ARCH=amd64; RUST_TARGET=x86_64-unknown-linux-gnu ;;
    aarch64) ARCH_FILE=arm64; DEB_ARCH=arm64; RUST_TARGET=aarch64-unknown-linux-gnu ;;
    *) die "unsupported architecture $(uname -m)" ;;
esac

# The desktop file with its Exec line filled in.
desktop_file() {
    local exec="$1" dest="$2"
    sed "s|@EXEC@|$exec|" "$PACKAGING/linux/muxus.desktop" > "$dest"
    chmod 0644 "$dest"
    if command -v desktop-file-validate >/dev/null 2>&1; then
        desktop-file-validate "$dest"
    fi
}

# hicolor icons under <root>/usr/share/icons.
install_icons() {
    local root="$1" size
    for size in "${ICON_SIZES[@]}"; do
        install -D -m 0644 "$PACKAGING/linux/icons/${size}x${size}.png" \
            "$root/usr/share/icons/hicolor/${size}x${size}/apps/muxus.png"
    done
}

# License and attribution files into <dir>: Muxus' own license, the attribution of bundled
# assets, the font and icon licenses, and every linked crate's license.
install_licenses() {
    local dir="$1"
    mkdir -p "$dir"
    install -m 0644 "$PACKAGING/LICENSE" "$dir/LICENSE"
    install -m 0644 "$ROOT/ATTRIBUTION.md" "$dir/ATTRIBUTION.md"
    local license
    for license in "$ROOT"/assets/fonts/*-LICENSE.txt "$ROOT"/assets/icons/LICENSE-*.txt; do
        [ -f "$license" ] && install -m 0644 "$license" "$dir/$(basename "$license")"
    done
    if [ ! -f "$OUT/THIRD_PARTY_LICENSES.txt" ] || [ "$ROOT/Cargo.lock" -nt "$OUT/THIRD_PARTY_LICENSES.txt" ]; then
        python3 -I "$PACKAGING/third_party_licenses.py" "$OUT/THIRD_PARTY_LICENSES.txt" --target "$RUST_TARGET"
    fi
    install -m 0644 "$OUT/THIRD_PARTY_LICENSES.txt" "$dir/THIRD_PARTY_LICENSES.txt"
}

# Base64 SHA-512 and size of a file, as release feeds list them.
feed_entry() {
    python3 -I - "$1" <<'PY'
import base64, hashlib, os, sys
path = sys.argv[1]
digest = hashlib.sha512()
with open(path, "rb") as handle:
    for chunk in iter(lambda: handle.read(1 << 20), b""):
        digest.update(chunk)
print(base64.b64encode(digest.digest()).decode(), os.path.getsize(path))
PY
}
