#!/usr/bin/env bash
#
# Regenerate the bundled icon set in assets/brand/ from assets/brand/logo.svg.
#
# Outputs:
#   assets/brand/icon_{16,24,32,48,64,128,256,512,1024}x{...}.png   (Linux)
#   assets/brand/icon.icns                                          (macOS)
#   assets/brand/icon.ico                                           (Windows)
#
# Requires:
#   - rsvg-convert  (brew install librsvg / apt install librsvg2-bin)
#   - iconutil      (ships with macOS Xcode CLT — needed for .icns)
#   - png2ico       (brew install png2ico / apt install png2ico)  OR
#     ImageMagick   (brew install imagemagick / apt install imagemagick)
#
# Usage: scripts/build_icons.sh

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
SRC="$REPO_ROOT/assets/brand/logo.svg"
OUT="$REPO_ROOT/assets/brand"

if [[ ! -f "$SRC" ]]; then
    echo "Error: $SRC not found" >&2
    exit 1
fi
if ! command -v rsvg-convert >/dev/null 2>&1; then
    echo "Error: rsvg-convert not found (brew install librsvg)" >&2
    exit 1
fi

SIZES=(16 24 32 48 64 128 256 512 1024)

echo "==> rendering PNGs from $SRC"
for s in "${SIZES[@]}"; do
    rsvg-convert -w "$s" -h "$s" "$SRC" -o "$OUT/icon_${s}x${s}.png"
done

# ─── macOS .icns ────────────────────────────────────────────────────────────
if command -v iconutil >/dev/null 2>&1; then
    echo "==> building $OUT/icon.icns"
    ICONSET="$(mktemp -d)/icon.iconset"
    mkdir -p "$ICONSET"
    # iconutil expects this naming scheme.
    cp "$OUT/icon_16x16.png"     "$ICONSET/icon_16x16.png"
    cp "$OUT/icon_32x32.png"     "$ICONSET/icon_16x16@2x.png"
    cp "$OUT/icon_32x32.png"     "$ICONSET/icon_32x32.png"
    cp "$OUT/icon_64x64.png"     "$ICONSET/icon_32x32@2x.png"
    cp "$OUT/icon_128x128.png"   "$ICONSET/icon_128x128.png"
    cp "$OUT/icon_256x256.png"   "$ICONSET/icon_128x128@2x.png"
    cp "$OUT/icon_256x256.png"   "$ICONSET/icon_256x256.png"
    cp "$OUT/icon_512x512.png"   "$ICONSET/icon_256x256@2x.png"
    cp "$OUT/icon_512x512.png"   "$ICONSET/icon_512x512.png"
    cp "$OUT/icon_1024x1024.png" "$ICONSET/icon_512x512@2x.png"
    iconutil -c icns -o "$OUT/icon.icns" "$ICONSET"
else
    echo "Skipping icon.icns (iconutil not found — macOS only)" >&2
fi

# ─── Windows .ico ───────────────────────────────────────────────────────────
if command -v png2ico >/dev/null 2>&1; then
    echo "==> building $OUT/icon.ico via png2ico"
    png2ico "$OUT/icon.ico" \
        "$OUT/icon_16x16.png" \
        "$OUT/icon_32x32.png" \
        "$OUT/icon_48x48.png" \
        "$OUT/icon_64x64.png" \
        "$OUT/icon_128x128.png" \
        "$OUT/icon_256x256.png"
elif command -v magick >/dev/null 2>&1; then
    echo "==> building $OUT/icon.ico via ImageMagick"
    magick \
        "$OUT/icon_16x16.png" \
        "$OUT/icon_32x32.png" \
        "$OUT/icon_48x48.png" \
        "$OUT/icon_64x64.png" \
        "$OUT/icon_128x128.png" \
        "$OUT/icon_256x256.png" \
        "$OUT/icon.ico"
else
    echo "Skipping icon.ico (neither png2ico nor ImageMagick found)" >&2
fi

echo "==> done"
