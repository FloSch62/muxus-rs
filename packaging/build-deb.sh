#!/usr/bin/env bash
# Build the Debian package from a release binary, laid out like the Electron package so
# either replaces the other: /opt/Muxus/muxus, /usr/bin/muxus through update-alternatives,
# muxus.desktop (also offered for ssh:// and telnet:// links), hicolor icons, licenses.
#
#   CARGO_TARGET_DIR=target-release cargo build --release -p muxus-app
#   MUXUS_BINARY=target-release/release/muxus packaging/build-deb.sh
#
# Needs dpkg-deb (no root: --root-owner-group); dpkg-shlibdeps, when present, computes the
# library dependencies.

source "$(dirname "${BASH_SOURCE[0]}")/common.sh"

PACKAGE_NAME="${MUXUS_DEB_NAME:-muxus}"
STAGE="$OUT/deb-root"
rm -rf "$STAGE"
mkdir -p "$STAGE/DEBIAN" "$STAGE/opt/Muxus" "$STAGE/usr/share/applications"

install -m 0755 "$BINARY" "$STAGE/opt/Muxus/muxus"
desktop_file /opt/Muxus/muxus "$STAGE/usr/share/applications/muxus.desktop"
install_icons "$STAGE"
install_licenses "$STAGE/opt/Muxus"

# Debian's copyright file: Muxus' license, then a pointer to the full list.
DOC="$STAGE/usr/share/doc/$PACKAGE_NAME"
mkdir -p "$DOC"
{
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: Muxus"
    echo "Source: https://github.com/FloSch62/muxus"
    echo
    echo "Files: *"
    echo "Copyright: 2026 Florian Schwarz"
    echo "License: MIT"
    sed 's/^$/./; s/^/ /' "$PACKAGING/LICENSE"
    echo
    echo "Comment: Bundled fonts, icons and linked Rust crates keep their own licenses;"
    echo " see /opt/Muxus/ATTRIBUTION.md and /opt/Muxus/THIRD_PARTY_LICENSES.txt."
} > "$DOC/copyright"
chmod 0644 "$DOC/copyright"

# Library dependencies of the binary, plus what it loads at run time: Wayland, and Vulkan or
# EGL for drawing.
DEPENDS="libc6, libgcc-s1, libxcb1, libxkbcommon0, libxkbcommon-x11-0, zlib1g"
if command -v dpkg-shlibdeps >/dev/null 2>&1; then
    SHLIBS="$(mktemp -d)"
    mkdir -p "$SHLIBS/debian"
    printf 'Source: muxus\n\nPackage: muxus\nArchitecture: any\n' > "$SHLIBS/debian/control"
    if computed="$(cd "$SHLIBS" && dpkg-shlibdeps -O "$STAGE/opt/Muxus/muxus" 2>/dev/null)"; then
        DEPENDS="${computed#shlibs:Depends=}"
    fi
    rm -rf "$SHLIBS"
fi
DEPENDS="$DEPENDS, libwayland-client0, libvulkan1 | libegl1"

INSTALLED_SIZE="$(du -sk --exclude=DEBIAN "$STAGE" | cut -f1)"
cat > "$STAGE/DEBIAN/control" <<EOF
Package: $PACKAGE_NAME
Version: $VERSION
Architecture: $DEB_ARCH
Maintainer: Florian Schwarz <schwarz.flori.88@gmail.com>
Installed-Size: $INSTALLED_SIZE
Depends: $DEPENDS
Recommends: xdg-utils
Section: net
Priority: optional
Homepage: https://github.com/FloSch62/muxus
Description: SSH, Telnet & serial client
 Muxus desktop app: SSH, Telnet, serial, SFTP, RDP and VNC sessions in tabs and
 split panes, with saved hosts, tunnels, workspaces and session history.
EOF
install -m 0755 "$PACKAGING/linux/postinst" "$STAGE/DEBIAN/postinst"
install -m 0755 "$PACKAGING/linux/postrm" "$STAGE/DEBIAN/postrm"

DEB="$OUT/muxus-$VERSION-linux-$DEB_ARCH.deb"
rm -f "$DEB"
dpkg-deb --root-owner-group -Zxz --build "$STAGE" "$DEB" >/dev/null
rm -rf "$STAGE"
echo "$DEB"
