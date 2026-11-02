#!/usr/bin/env bash
# Cross-builds openzap.exe from Linux/WSL (MinGW) and assembles a folder
# that can be run on Windows: openzap.exe + libmpv-2.dll.
#
# Usage: scripts/build-windows.sh [DEST]      (default: dist/windows)
#   e.g. scripts/build-windows.sh /mnt/d/Files/openzap-test
#
# Needs: rustup target x86_64-pc-windows-gnu, mingw-w64, curl, 7z (package 7zip).
# Release builds are made by CI with MSVC; this is for quick local testing.
set -euo pipefail

cd "$(dirname "$0")/.."
DEST="${1:-dist/windows}"
TARGET=x86_64-pc-windows-gnu

need() { command -v "$1" >/dev/null || { echo "missing: $1 ($2)" >&2; exit 1; }; }
need cargo "install Rust"
need x86_64-w64-mingw32-gcc "sudo apt install mingw-w64"
need curl "sudo apt install curl"
need sha256sum "coreutils"

# The libmpv pin lives in one place: the PowerShell script CI uses.
pin() { sed -n "s/^\$$1 = '\(.*\)'.*/\1/p" scripts/fetch-libmpv-windows.ps1; }
TAG=$(pin Tag); ASSET=$(pin Asset); SHA256=$(pin Sha256)
[ -n "$TAG" ] && [ -n "$ASSET" ] && [ -n "$SHA256" ] || { echo "cannot read libmpv pin" >&2; exit 1; }

rustup target list --installed | grep -qx "$TARGET" || rustup target add "$TARGET"
cargo build --release --locked --target "$TARGET"

# Download once into target/ and verify, reusing the cached archive later.
CACHE="target/libmpv-windows"
mkdir -p "$CACHE"
ARCHIVE="$CACHE/$ASSET"
if ! echo "$SHA256  $ARCHIVE" | sha256sum -c --status 2>/dev/null; then
    echo "downloading $ASSET"
    curl -fL --retry 3 -o "$ARCHIVE.part" \
        "https://github.com/shinchiro/mpv-winbuild-cmake/releases/download/$TAG/$ASSET"
    mv "$ARCHIVE.part" "$ARCHIVE"
    echo "$SHA256  $ARCHIVE" | sha256sum -c --status || {
        rm -f "$ARCHIVE"; echo "SHA-256 mismatch for $ASSET" >&2; exit 1; }
fi
if [ ! -s "$CACHE/libmpv-2.dll" ]; then
    SEVENZIP=$(command -v 7z || command -v 7zz || true)
    [ -n "$SEVENZIP" ] || { echo "missing: 7z (sudo apt install 7zip)" >&2; exit 1; }
    "$SEVENZIP" x "$ARCHIVE" "-o$CACHE" libmpv-2.dll -y >/dev/null
fi

mkdir -p "$DEST"
cp "target/$TARGET/release/openzap.exe" "$DEST/"
[ -s "$CACHE/libmpv-2.dll" ] || { echo "libmpv-2.dll was not extracted" >&2; exit 1; }
cp -u "$CACHE/libmpv-2.dll" "$DEST/"
echo "ready: $DEST/openzap.exe"
