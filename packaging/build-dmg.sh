#!/usr/bin/env bash
# Build Muxus.app and a disk image from a release binary (ideally a universal one, made with
# lipo from the x86_64 and aarch64 builds). macOS only: needs sips, iconutil, codesign and
# hdiutil. The app is signed ad hoc, not with a Developer ID, so Gatekeeper asks once.
#
#   cargo build --release -p muxus-app --target aarch64-apple-darwin
#   cargo build --release -p muxus-app --target x86_64-apple-darwin
#   lipo -create -output target/muxus target/{aarch64,x86_64}-apple-darwin/release/muxus
#   MUXUS_BINARY=target/muxus packaging/build-dmg.sh
#
# Inputs (environment): MUXUS_BINARY, MUXUS_VERSION, MUXUS_PACKAGE_DIR, as for build-deb.sh.

set -euo pipefail
umask 022

PACKAGING="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$PACKAGING/.." && pwd)"

die() {
    echo "error: $*" >&2
    exit 1
}

VERSION="${MUXUS_VERSION:-$(sed -n 's/^version = "\(.*\)"$/\1/p' "$ROOT/Cargo.toml" | head -n 1)}"
BINARY="${MUXUS_BINARY:-${CARGO_TARGET_DIR:-$ROOT/target}/release/muxus}"
OUT="${MUXUS_PACKAGE_DIR:-$(dirname "$BINARY")/packages}"

[ -n "$VERSION" ] || die "could not read the version from $ROOT/Cargo.toml"
[ -x "$BINARY" ] || die "no release binary at $BINARY"
mkdir -p "$OUT"

case "$(lipo -archs "$BINARY")" in
    *x86_64*arm64* | *arm64*x86_64*) ARCH_FILE=universal ;;
    *arm64*) ARCH_FILE=arm64 ;;
    *x86_64*) ARCH_FILE=x86_64 ;;
    *) die "unexpected architectures in $BINARY" ;;
esac

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APP="$STAGE/dmg/Muxus.app"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

install -m 0755 "$BINARY" "$APP/Contents/MacOS/muxus"

# The icon, from the same artwork as the Linux hicolor icons.
ICONSET="$STAGE/Muxus.iconset"
mkdir -p "$ICONSET"
for size in 16 32 128 256 512; do
    sips -z "$size" "$size" "$PACKAGING/linux/icons/1024x1024.png" \
        --out "$ICONSET/icon_${size}x${size}.png" >/dev/null
    double=$((size * 2))
    sips -z "$double" "$double" "$PACKAGING/linux/icons/1024x1024.png" \
        --out "$ICONSET/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns "$ICONSET" -o "$APP/Contents/Resources/Muxus.icns"

cat > "$APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key><string>Muxus</string>
    <key>CFBundleDisplayName</key><string>Muxus</string>
    <key>CFBundleIdentifier</key><string>io.github.flosch62.muxus</string>
    <key>CFBundleExecutable</key><string>muxus</string>
    <key>CFBundleIconFile</key><string>Muxus</string>
    <key>CFBundlePackageType</key><string>APPL</string>
    <key>CFBundleShortVersionString</key><string>$VERSION</string>
    <key>CFBundleVersion</key><string>$VERSION</string>
    <key>CFBundleInfoDictionaryVersion</key><string>6.0</string>
    <key>LSApplicationCategoryType</key><string>public.app-category.developer-tools</string>
    <key>LSMinimumSystemVersion</key><string>11.0</string>
    <key>NSHighResolutionCapable</key><true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key><true/>
    <key>CFBundleURLTypes</key>
    <array>
        <dict>
            <key>CFBundleURLName</key><string>Remote shell</string>
            <key>CFBundleURLSchemes</key><array><string>ssh</string><string>telnet</string></array>
        </dict>
    </array>
</dict>
</plist>
EOF

RESOURCES="$APP/Contents/Resources"
install -m 0644 "$PACKAGING/LICENSE" "$RESOURCES/LICENSE"
install -m 0644 "$ROOT/ATTRIBUTION.md" "$RESOURCES/ATTRIBUTION.md"
for license in "$ROOT"/assets/fonts/*-LICENSE.txt "$ROOT"/assets/icons/LICENSE-*.txt; do
    [ -f "$license" ] && install -m 0644 "$license" "$RESOURCES/$(basename "$license")"
done
python3 -I "$PACKAGING/third_party_licenses.py" "$RESOURCES/THIRD_PARTY_LICENSES.txt" \
    --target aarch64-apple-darwin

codesign --force --deep --sign - "$APP"

ln -s /Applications "$STAGE/dmg/Applications"
DMG="$OUT/muxus-$VERSION-mac-$ARCH_FILE.dmg"
rm -f "$DMG"
hdiutil create -volname Muxus -srcfolder "$STAGE/dmg" -fs HFS+ -format UDZO "$DMG" >/dev/null
echo "$DMG"
