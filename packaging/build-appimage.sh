#!/usr/bin/env bash
# Build the AppImage from a release binary.
#
#   CARGO_TARGET_DIR=target-release cargo build --release -p muxus-app
#   MUXUS_BINARY=target-release/release/muxus packaging/build-appimage.sh
#
# With appimagetool on PATH it is used. Otherwise the AppImage is assembled by hand, which
# is all appimagetool does for us: a SquashFS of the AppDir (mksquashfs, from squashfs-tools)
# appended to the AppImage type 2 runtime. The runtime comes from APPIMAGE_RUNTIME, or is
# downloaded once from the AppImage project's releases into $OUT/.cache.
#
# The AppImage starts `AppRun`, which runs usr/bin/muxus. The runtime sets APPIMAGE, which
# Muxus uses for link registration and to replace the file on in-app updates.

source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

RUNTIME_URL="https://github.com/AppImage/type2-runtime/releases/download/continuous/runtime-$(uname -m)"
APPDIR="$OUT/Muxus.AppDir"
APPIMAGE="$OUT/muxus-$VERSION-linux-$ARCH_FILE.AppImage"

rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/share/applications"
install -m 0755 "$BINARY" "$APPDIR/usr/bin/muxus"
install -m 0755 "$PACKAGING/linux/AppRun" "$APPDIR/AppRun"
desktop_file muxus "$APPDIR/usr/share/applications/muxus.desktop"
cp "$APPDIR/usr/share/applications/muxus.desktop" "$APPDIR/muxus.desktop"
install_icons "$APPDIR"
install -m 0644 "$PACKAGING/linux/icons/256x256.png" "$APPDIR/muxus.png"
ln -sf muxus.png "$APPDIR/.DirIcon"
install_licenses "$APPDIR/usr/share/doc/muxus"

rm -f "$APPIMAGE"
if command -v appimagetool >/dev/null 2>&1; then
    ARCH="$(uname -m)" appimagetool --no-appstream "$APPDIR" "$APPIMAGE"
else
    command -v mksquashfs >/dev/null 2>&1 || die "neither appimagetool nor mksquashfs is installed"
    RUNTIME="${APPIMAGE_RUNTIME:-$OUT/.cache/runtime-$(uname -m)}"
    if [ ! -f "$RUNTIME" ]; then
        [ -z "${APPIMAGE_RUNTIME:-}" ] || die "APPIMAGE_RUNTIME=$APPIMAGE_RUNTIME does not exist"
        mkdir -p "$(dirname "$RUNTIME")"
        echo "downloading the AppImage runtime from $RUNTIME_URL" >&2
        curl --fail --location --silent --show-error --output "$RUNTIME.part" "$RUNTIME_URL"
        mv "$RUNTIME.part" "$RUNTIME"
    fi
    # An ELF file with the AppImage type 2 magic (bytes 41 49 02) at offset 8.
    [ "$(head -c 4 "$RUNTIME" | od -An -c | tr -d ' ')" = '177ELF' ] || die "$RUNTIME is not an ELF runtime"
    [ "$(dd if="$RUNTIME" bs=1 skip=8 count=3 2>/dev/null | od -An -tx1 | tr -d ' ')" = "414902" ] \
        || die "$RUNTIME is not an AppImage type 2 runtime"
    echo "runtime sha256: $(sha256sum "$RUNTIME" | cut -d' ' -f1)" >&2
    SQUASHFS="$OUT/muxus.squashfs"
    rm -f "$SQUASHFS"
    mksquashfs "$APPDIR" "$SQUASHFS" -root-owned -noappend -comp zstd -quiet -no-progress >/dev/null
    cat "$RUNTIME" "$SQUASHFS" > "$APPIMAGE"
    rm -f "$SQUASHFS"
fi
chmod 0755 "$APPIMAGE"
rm -rf "$APPDIR"
echo "$APPIMAGE"
