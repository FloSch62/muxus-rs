#!/usr/bin/env bash
# Write the Linux release feed (latest-linux.yml) for packages built by build-deb.sh and
# build-appimage.sh: the file the in-app updater reads from a release (muxus_session::updater).
# The AppImage comes first, so clients that read only `path` update the AppImage.
#
#   packaging/release-feed.sh [PACKAGE_DIR]

source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

DIR="${1:-$OUT}"
APPIMAGE="muxus-$VERSION-linux-$ARCH_FILE.AppImage"
DEB="muxus-$VERSION-linux-$DEB_ARCH.deb"
[ -f "$DIR/$APPIMAGE" ] || die "missing $DIR/$APPIMAGE"
FEED="$DIR/latest-linux.yml"
[ "$ARCH_FILE" = x86_64 ] || FEED="$DIR/latest-linux-$ARCH_FILE.yml"

{
    echo "version: \"$VERSION\""
    echo "files:"
    for file in "$APPIMAGE" "$DEB"; do
        [ -f "$DIR/$file" ] || continue
        read -r sha512 size < <(feed_entry "$DIR/$file")
        echo "  - url: \"$file\""
        echo "    sha512: \"$sha512\""
        echo "    size: $size"
    done
    read -r sha512 _ < <(feed_entry "$DIR/$APPIMAGE")
    echo "path: \"$APPIMAGE\""
    echo "sha512: \"$sha512\""
    echo "releaseDate: \"$(date -u +%Y-%m-%dT%H:%M:%S.000Z)\""
} > "$FEED"
echo "$FEED"
