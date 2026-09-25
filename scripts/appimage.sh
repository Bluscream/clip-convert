#!/usr/bin/env bash
# Builds a portable AppImage of clip-convert.
#
# The binaries are compiled inside an old distribution on purpose. A program
# linked against glibc 2.43 will not start on a system with 2.31, but one
# linked against 2.31 runs on both — so the build happens in Ubuntu 20.04
# and the result works on anything from about 2020 onwards.
#
# Everything else that can be avoided is: OpenSSL is vendored into the binary,
# and Wayland, X11, xkbcommon and OpenGL are opened by name at runtime rather
# than linked, so the only library bundled here is the one enigo links
# directly.
set -euo pipefail

PROJECT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
BUILD_IMAGE="${LCC_BUILD_IMAGE:-docker.io/library/ubuntu:20.04}"
OUT_DIR="${LCC_OUT_DIR:-$PROJECT_DIR/dist}"
ARCH="${ARCH:-x86_64}"

# Set when the binaries are already built for the target and only packaging is
# wanted — which is how the CI workflow uses this, having built in a container
# of its own.
SKIP_BUILD="${LCC_SKIP_BUILD:-0}"
BIN_DIR="${LCC_BIN_DIR:-$PROJECT_DIR/target/appimage/release}"

usage() {
    cat <<'USAGE'
Usage: appimage.sh [--skip-build]

  --skip-build   Package binaries that are already in target/appimage/release
                 instead of compiling them.

Environment:
  LCC_BUILD_IMAGE  Container image to build in (default: ubuntu:20.04)
  LCC_OUT_DIR      Where to write the AppImage (default: ./dist)
  LCC_BIN_DIR      Where the binaries are (default: target/appimage/release)
USAGE
}

while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "unknown option: $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

cd "$PROJECT_DIR"
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"

if [[ $SKIP_BUILD -eq 0 ]]; then
    echo "==> building in $BUILD_IMAGE"
    # A separate target directory: the host's target/ holds objects linked
    # against the host's glibc, and mixing them silently produces a binary that
    # only runs here.
    mkdir -p "$PROJECT_DIR/target/appimage" "$PROJECT_DIR/target/appimage-cargo"
    podman run --rm \
        -v "$PROJECT_DIR:/src:z" \
        -v "$PROJECT_DIR/target/appimage-cargo:/cargo:z" \
        -e CARGO_HOME=/cargo \
        -e CARGO_TARGET_DIR=/src/target/appimage \
        -w /src \
        "$BUILD_IMAGE" \
        bash -euc '
            export DEBIAN_FRONTEND=noninteractive
            apt-get update -qq
            apt-get install -y -qq --no-install-recommends \
                build-essential curl ca-certificates pkg-config perl make \
                libxkbcommon-dev >/dev/null
            if [ ! -x /cargo/bin/cargo ]; then
                curl --proto "=https" --tlsv1.2 -sSf https://sh.rustup.rs \
                    | sh -s -- -y --profile minimal --default-toolchain stable >/dev/null
            fi
            export PATH=/cargo/bin:$PATH
            cargo build --workspace --release
        '
fi

for binary in clip-convert clip-convert-dialog; do
    if [[ ! -x "$BIN_DIR/$binary" ]]; then
        echo "missing $BIN_DIR/$binary — build first, or point LCC_BIN_DIR at them" >&2
        exit 1
    fi
done

echo "==> assembling the AppDir"
APPDIR="$PROJECT_DIR/target/AppDir"
rm -rf "$APPDIR"
mkdir -p "$APPDIR/usr/bin" "$APPDIR/usr/lib" \
         "$APPDIR/usr/share/applications" \
         "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
         "$APPDIR/usr/share/icons/hicolor/scalable/apps"

install -m755 "$BIN_DIR/clip-convert" "$APPDIR/usr/bin/"
install -m755 "$BIN_DIR/clip-convert-dialog" "$APPDIR/usr/bin/"
strip "$APPDIR/usr/bin/clip-convert" "$APPDIR/usr/bin/clip-convert-dialog" 2>/dev/null || true

install -m644 packaging/clip-convert.desktop "$APPDIR/usr/share/applications/"
install -m644 packaging/clip-convert.svg "$APPDIR/usr/share/icons/hicolor/scalable/apps/"
magick -background none packaging/clip-convert.svg -resize 256x256 \
    "$APPDIR/usr/share/icons/hicolor/256x256/apps/clip-convert.png"

# The three files appimagetool expects at the root of the AppDir.
cp "$APPDIR/usr/share/applications/clip-convert.desktop" "$APPDIR/"
cp "$APPDIR/usr/share/icons/hicolor/256x256/apps/clip-convert.png" "$APPDIR/"
ln -sf clip-convert.png "$APPDIR/.DirIcon"

# Bundle only what is genuinely linked, taken from the build container so it is
# the old version too.
echo "==> bundling libraries"
for library in $(ldd "$APPDIR/usr/bin/clip-convert" | awk '/=>/ {print $3}'); do
    case "$(basename "$library")" in
        libxkbcommon.so*)
            cp -L "$library" "$APPDIR/usr/lib/" ;;
    esac
done

cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
# The bundled libraries come second: a system copy of libxkbcommon is
# preferable when there is one, because it matches the compositor's.
HERE="$(dirname "$(readlink -f "$0")")"
export LD_LIBRARY_PATH="${LD_LIBRARY_PATH:+$LD_LIBRARY_PATH:}$HERE/usr/lib"
exec "$HERE/usr/bin/clip-convert" "$@"
APPRUN
chmod +x "$APPDIR/AppRun"

echo "==> packing"
mkdir -p "$OUT_DIR"
OUTPUT="$OUT_DIR/clip-convert-$VERSION-$ARCH.AppImage"
# No desktop integration prompt and no update information: this is a plain
# portable binary, and Gear Lever handles integration on this desktop.
ARCH="$ARCH" appimagetool --no-appstream "$APPDIR" "$OUTPUT" >/dev/null

echo "==> built $OUTPUT"
ls -lh "$OUTPUT"
