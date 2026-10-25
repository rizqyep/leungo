#!/usr/bin/env bash
# Builds Leungo-<version>-x86_64.AppImage from target/release/leungo. Run on Linux.
# Usage: packaging/linux/build-appimage.sh <version> [output-dir]
#
# The binary needs the system libpulse (every desktop with PipeWire or
# PulseAudio has it), so the AppImage does not bundle it; a bundled libpulse
# would look for libpulsecommon in the build machine's path and fail elsewhere.
set -euo pipefail

VERSION="${1:?version required}"
OUT="${2:-dist}"
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
BIN="$ROOT/target/release/leungo"
[ -x "$BIN" ] || { echo "build first: cargo build --release -p leungo-cli" >&2; exit 1; }

APPIMAGETOOL_VERSION=1.9.1
APPIMAGETOOL_SHA256=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
RUNTIME_VERSION=20251108
RUNTIME_SHA256=2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# Fetch a pinned tool once into target/, checking it against the hash above.
fetch() {
  url="$1" sha="$2" dest="$3" label="$4"
  if [ ! -x "$dest" ]; then
    echo "downloading $label" >&2
    mkdir -p "$(dirname "$dest")"
    curl -fL --retry 3 -o "$dest.tmp" "$url"
    printf '%s  %s\n' "$sha" "$dest.tmp" | sha256sum -c -
    chmod +x "$dest.tmp"
    mv "$dest.tmp" "$dest"
  fi
}

TOOL="${APPIMAGETOOL:-}"
[ -n "$TOOL" ] || TOOL="$(command -v appimagetool || true)"
if [ -z "$TOOL" ]; then
  TOOL="$ROOT/target/appimagetool-$APPIMAGETOOL_VERSION"
  fetch \
    "https://github.com/AppImage/appimagetool/releases/download/$APPIMAGETOOL_VERSION/appimagetool-x86_64.AppImage" \
    "$APPIMAGETOOL_SHA256" "$TOOL" "appimagetool $APPIMAGETOOL_VERSION"
fi

# The runtime mounts the payload. Pinned so releases stay reproducible; the
# 2025 runtime links FUSE statically, so no libfuse2 is needed on the target.
RUNTIME="${APPIMAGE_RUNTIME:-}"
if [ -z "$RUNTIME" ]; then
  RUNTIME="$ROOT/target/appimage-runtime-$RUNTIME_VERSION-x86_64"
  fetch \
    "https://github.com/AppImage/type2-runtime/releases/download/$RUNTIME_VERSION/runtime-x86_64" \
    "$RUNTIME_SHA256" "$RUNTIME" "AppImage runtime $RUNTIME_VERSION"
fi

STAGE="$(mktemp -d)"
trap 'rm -rf "$STAGE"' EXIT
APPDIR="$STAGE/Leungo.AppDir"
mkdir -p \
  "$APPDIR/usr/bin" \
  "$APPDIR/usr/share/applications" \
  "$APPDIR/usr/share/icons/hicolor/256x256/apps" \
  "$APPDIR/usr/share/icons/hicolor/48x48/apps" \
  "$APPDIR/usr/share/doc/leungo" \
  "$OUT"

cp "$BIN" "$APPDIR/usr/bin/leungo"
cp "$ROOT/packaging/linux/leungo.desktop" "$APPDIR/leungo.desktop"
cp "$ROOT/packaging/linux/leungo.desktop" "$APPDIR/usr/share/applications/leungo.desktop"
cp "$ROOT/assets/brand/png/leungo-256.png" "$APPDIR/leungo.png"
cp "$ROOT/assets/brand/png/leungo-256.png" "$APPDIR/usr/share/icons/hicolor/256x256/apps/leungo.png"
cp "$ROOT/assets/brand/png/leungo-48.png" "$APPDIR/usr/share/icons/hicolor/48x48/apps/leungo.png"
cp "$ROOT/LICENSE" "$APPDIR/usr/share/doc/leungo/LICENSE"
ln -s leungo.png "$APPDIR/.DirIcon"

# Opened from a menu (no arguments) it shows the dashboard; CLI commands pass through.
cat > "$APPDIR/AppRun" <<'APPRUN'
#!/bin/sh
HERE="$(dirname "$(readlink -f "$0")")"
[ "$#" -eq 0 ] && set -- gui
exec "$HERE/usr/bin/leungo" "$@"
APPRUN
chmod 755 "$APPDIR/AppRun" "$APPDIR/usr/bin/leungo"

IMAGE="$OUT/Leungo-${VERSION}-x86_64.AppImage"
rm -f "$IMAGE"
# appimagetool ships as an AppImage too; extracting it avoids needing a
# fusermount helper on CI machines and in containers.
APPIMAGE_EXTRACT_AND_RUN=1 ARCH=x86_64 \
  "$TOOL" --no-appstream --runtime-file "$RUNTIME" "$APPDIR" "$IMAGE"
(cd "$OUT" && sha256sum "$(basename "$IMAGE")" > "$(basename "$IMAGE").sha256")
