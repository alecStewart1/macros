#!/usr/bin/env bash
#
# Build a release macOS .app bundle for Macros, sign it, and (optionally)
# notarize + staple it locally so it passes Gatekeeper on other Macs.
#
# Usage:
#   scripts/build_macos.sh [--no-build] [--notarize]
#
# Flags:
#   --no-build    Skip the cargo build; bundle the binary already on disk.
#   --notarize    After signing, submit the bundle to Apple's notary service
#                 and staple the ticket. Requires a real Developer ID identity
#                 (CODESIGN_IDENTITY, not ad-hoc "-") and one set of notary
#                 credentials (see below).
#
# Environment variables:
#   CODESIGN_IDENTITY   Codesigning identity (e.g. "Developer ID Application:
#                       Your Name (TEAMID)") to pass to `codesign --sign`.
#                       Defaults to "-" (ad-hoc / unsigned).
#   ENTITLEMENTS        Optional path to an entitlements .plist forwarded to
#                       `codesign --entitlements`.
#   APP_NAME            Bundle display name. Default "Macros".
#   BUNDLE_ID           CFBundleIdentifier. Default "dev.macros.macros".
#   RUST_TARGET         Optional Rust target triple to cross-compile for, e.g.
#                       "x86_64-apple-darwin" to build an Intel bundle on an
#                       Apple Silicon host. Empty (default) builds for the host
#                       arch. When set, the binary/bundle live under
#                       target/<triple>/release/ instead of target/release/.
#
# Notarization credentials (only with --notarize; pick ONE set, checked in
# this order):
#   1. Keychain profile (simplest for local use — set it up once with
#      `xcrun notarytool store-credentials`):
#        NOTARY_PROFILE        Stored notarytool profile name.
#   2. App Store Connect API key:
#        APPLE_API_KEY_P8      Path to the AuthKey_XXXX.p8 file.
#        APPLE_API_KEY_ID      The key's Key ID.
#        APPLE_API_ISSUER_ID   The Issuer ID (a UUID).
#   3. Apple ID + app-specific password:
#        APPLE_ID                     Your Apple ID email.
#        APPLE_APP_SPECIFIC_PASSWORD  An app-specific password.
#        APPLE_TEAM_ID                Your 10-char Team ID.
#
# Output: target/[<triple>/]release/Macros.app (path printed to stdout).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
APP_NAME="${APP_NAME:-Macros}"
BUNDLE_ID="${BUNDLE_ID:-dev.macros.macros}"
CODESIGN_IDENTITY="${CODESIGN_IDENTITY:-}"
ENTITLEMENTS="${ENTITLEMENTS:-}"
RUST_TARGET="${RUST_TARGET:-}"

# ── Flags ────────────────────────────────────────────────────────────────────
DO_BUILD=1
DO_NOTARIZE=0
for arg in "$@"; do
    case "$arg" in
        --no-build) DO_BUILD=0 ;;
        --notarize) DO_NOTARIZE=1 ;;
        -h|--help)
            sed -n '2,44p' "$0" | sed 's/^# \{0,1\}//'
            exit 0
            ;;
        *)
            echo "Error: unknown argument '$arg' (try --help)" >&2
            exit 1
            ;;
    esac
done

# Cargo package name (and therefore the produced binary) is `macros`.
BIN_NAME="macros"
ICON_SRC="$REPO_ROOT/assets/brand/icon.icns"

# ── Resolve cargo's target directory ─────────────────────────────────────────
# Precedence: $CARGO_TARGET_DIR > [build] target-dir in .cargo/config.toml >
# default $REPO_ROOT/target.
resolve_target_dir() {
    if [[ -n "${CARGO_TARGET_DIR:-}" ]]; then
        echo "$CARGO_TARGET_DIR"
        return
    fi
    local cfg="$REPO_ROOT/.cargo/config.toml"
    if [[ -f "$cfg" ]]; then
        local dir
        dir="$(awk '
            /^\[/ { in_build = ($0 ~ /^\[build\]/); next }
            in_build && /^[[:space:]]*target-dir[[:space:]]*=/ {
                sub(/^[^=]*=[[:space:]]*/, "")
                gsub(/^"|"$/, "")
                print
                exit
            }
        ' "$cfg")"
        if [[ -n "$dir" ]]; then
            if [[ "$dir" != /* ]]; then
                dir="$REPO_ROOT/$dir"
            fi
            echo "$dir"
            return
        fi
    fi
    echo "$REPO_ROOT/target"
}

CARGO_TARGET_DIR_RESOLVED="$(resolve_target_dir)"
# A cross-compile (`--target <triple>`) nests build output under a per-triple
# subdir; a host build does not. Mirror that here so we look in the right place.
if [[ -n "$RUST_TARGET" ]]; then
    TARGET_DIR="$CARGO_TARGET_DIR_RESOLVED/$RUST_TARGET/release"
else
    TARGET_DIR="$CARGO_TARGET_DIR_RESOLVED/release"
fi
BIN_PATH="$TARGET_DIR/$BIN_NAME"
APP_BUNDLE="$TARGET_DIR/$APP_NAME.app"

# Read version from the root Cargo.toml [package] table (line: `version = "x.y.z"`).
VERSION="$(awk '
    /^\[/ { sect = ($0 ~ /^\[package\]/) ? "pkg" : ""; next }
    sect == "pkg" && /^version *= *"/ {
        gsub(/^[^"]*"|"[^"]*$/, ""); print; exit
    }
' "$REPO_ROOT/Cargo.toml")"
VERSION="${VERSION:-0.0.0}"

# ── 1. Build (skipped if --no-build) ─────────────────────────────────────────
echo "==> target dir: $CARGO_TARGET_DIR_RESOLVED"
if [[ "$DO_BUILD" -eq 1 ]]; then
    if [[ -n "$RUST_TARGET" ]]; then
        echo "==> rustup target add $RUST_TARGET"
        rustup target add "$RUST_TARGET"
        echo "==> cargo build --release --bin $BIN_NAME --target $RUST_TARGET"
        (cd "$REPO_ROOT" && cargo build --release --bin "$BIN_NAME" --target "$RUST_TARGET")
    else
        echo "==> cargo build --release --bin $BIN_NAME"
        (cd "$REPO_ROOT" && cargo build --release --bin "$BIN_NAME")
    fi
fi

if [[ ! -x "$BIN_PATH" ]]; then
    echo "Error: release binary not found at $BIN_PATH" >&2
    exit 1
fi

# ── 2. Layout ────────────────────────────────────────────────────────────────
echo "==> assembling $APP_BUNDLE"
rm -rf "$APP_BUNDLE"
mkdir -p "$APP_BUNDLE/Contents/MacOS"
mkdir -p "$APP_BUNDLE/Contents/Resources"

cp "$BIN_PATH" "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"
chmod +x "$APP_BUNDLE/Contents/MacOS/$BIN_NAME"

if [[ -f "$ICON_SRC" ]]; then
    cp "$ICON_SRC" "$APP_BUNDLE/Contents/Resources/icon.icns"
else
    echo "Warning: icon $ICON_SRC not found — bundle will use the default icon" >&2
fi

# Ship bundled third-party license texts alongside the binary. Macros embeds the
# Lilex editor font (SIL OFL); include its license. Best-effort: a missing file
# is a warning, not a hard failure.
LICENSES_DIR="$APP_BUNDLE/Contents/Resources/Licenses"
mkdir -p "$LICENSES_DIR"
declare -a LICENSE_PAIRS=(
    "LICENSE|COPYING"
    "assets/fonts/Lilex-OFL.txt|OFL-Lilex.txt"
)
for pair in "${LICENSE_PAIRS[@]}"; do
    src="${pair%%|*}"
    dst="${pair##*|}"
    if [[ -f "$REPO_ROOT/$src" ]]; then
        cp "$REPO_ROOT/$src" "$LICENSES_DIR/$dst"
    else
        echo "Warning: license file $REPO_ROOT/$src not found — skipping" >&2
    fi
done

# ── 3. Info.plist ────────────────────────────────────────────────────────────
cat > "$APP_BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>$APP_NAME</string>
    <key>CFBundleDisplayName</key>
    <string>$APP_NAME</string>
    <key>CFBundleExecutable</key>
    <string>$BIN_NAME</string>
    <key>CFBundleIdentifier</key>
    <string>$BUNDLE_ID</string>
    <key>CFBundleVersion</key>
    <string>$VERSION</string>
    <key>CFBundleShortVersionString</key>
    <string>$VERSION</string>
    <key>CFBundleIconFile</key>
    <string>icon.icns</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleInfoDictionaryVersion</key>
    <string>6.0</string>
    <key>LSMinimumSystemVersion</key>
    <string>11.0</string>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSSupportsAutomaticGraphicsSwitching</key>
    <true/>
    <key>NSHumanReadableCopyright</key>
    <string>$APP_NAME $VERSION. Copyright (C) Likelihood LLC. Licensed under GPL-3.0-or-later.</string>
</dict>
</plist>
PLIST

# ── 4. Sign ──────────────────────────────────────────────────────────────────
SIGN_ID="${CODESIGN_IDENTITY:--}"
if [[ "$SIGN_ID" == "-" ]]; then
    echo "==> codesign (ad-hoc)"
else
    echo "==> codesign --sign \"$SIGN_ID\""
fi

codesign_args=(
    --force
    --options runtime
    --sign "$SIGN_ID"
)
# Ad-hoc signing (local/dev) skips the secure timestamp for speed. A real
# Developer ID identity MUST use `--timestamp` so the signature can be
# notarized — Apple's notary service rejects signatures without a secure
# timestamp.
if [[ "$SIGN_ID" == "-" ]]; then
    codesign_args+=(--timestamp=none)
else
    codesign_args+=(--timestamp)
fi
if [[ -n "$ENTITLEMENTS" ]]; then
    if [[ ! -f "$ENTITLEMENTS" ]]; then
        echo "Error: ENTITLEMENTS=$ENTITLEMENTS not found" >&2
        exit 1
    fi
    codesign_args+=(--entitlements "$ENTITLEMENTS")
fi

codesign "${codesign_args[@]}" "$APP_BUNDLE"

echo "==> verifying signature"
codesign --verify --deep --strict --verbose=2 "$APP_BUNDLE" || {
    echo "Error: signature verification failed" >&2
    exit 1
}

# ── 5. Notarize + staple (optional, --notarize) ──────────────────────────────
if [[ "$DO_NOTARIZE" -eq 1 ]]; then
    # Notarization requires a real Developer ID signature; Apple rejects ad-hoc
    # signed bundles. Fail early with a clear message rather than after upload.
    if [[ "$SIGN_ID" == "-" ]]; then
        echo "Error: --notarize requires a real Developer ID identity. Set CODESIGN_IDENTITY (got ad-hoc '-')." >&2
        exit 1
    fi
    command -v xcrun >/dev/null 2>&1 || {
        echo "Error: xcrun not found — install Xcode or the Command Line Tools." >&2
        exit 1
    }

    # Pick a credential set (keychain profile > API key > Apple ID).
    notary_creds=()
    if [[ -n "${NOTARY_PROFILE:-}" ]]; then
        notary_creds=(--keychain-profile "$NOTARY_PROFILE")
        echo "==> notarizing via keychain profile '$NOTARY_PROFILE'"
    elif [[ -n "${APPLE_API_KEY_P8:-}" && -n "${APPLE_API_KEY_ID:-}" && -n "${APPLE_API_ISSUER_ID:-}" ]]; then
        if [[ ! -f "$APPLE_API_KEY_P8" ]]; then
            echo "Error: APPLE_API_KEY_P8=$APPLE_API_KEY_P8 not found" >&2
            exit 1
        fi
        notary_creds=(--key "$APPLE_API_KEY_P8" --key-id "$APPLE_API_KEY_ID" --issuer "$APPLE_API_ISSUER_ID")
        echo "==> notarizing via App Store Connect API key"
    elif [[ -n "${APPLE_ID:-}" && -n "${APPLE_APP_SPECIFIC_PASSWORD:-}" && -n "${APPLE_TEAM_ID:-}" ]]; then
        notary_creds=(--apple-id "$APPLE_ID" --password "$APPLE_APP_SPECIFIC_PASSWORD" --team-id "$APPLE_TEAM_ID")
        echo "==> notarizing via Apple ID '$APPLE_ID'"
    else
        echo "Error: --notarize given but no notary credentials found." >&2
        echo "Set one of: NOTARY_PROFILE | APPLE_API_KEY_P8+APPLE_API_KEY_ID+APPLE_API_ISSUER_ID | APPLE_ID+APPLE_APP_SPECIFIC_PASSWORD+APPLE_TEAM_ID" >&2
        exit 1
    fi

    # notarytool wants a zip/dmg/pkg, not a raw .app. ditto with --keepParent is
    # Apple's recommended archive format for submission.
    NOTARIZE_ZIP="$TARGET_DIR/$APP_NAME-notarize.zip"
    rm -f "$NOTARIZE_ZIP"
    echo "==> packaging $NOTARIZE_ZIP"
    ditto -c -k --keepParent "$APP_BUNDLE" "$NOTARIZE_ZIP"

    # --wait blocks until Apple finishes; non-zero exit on rejection.
    echo "==> xcrun notarytool submit (this can take a few minutes)"
    xcrun notarytool submit "$NOTARIZE_ZIP" "${notary_creds[@]}" --wait
    rm -f "$NOTARIZE_ZIP"

    # Staple the ticket into the bundle so it validates offline, then verify.
    echo "==> stapling ticket"
    xcrun stapler staple "$APP_BUNDLE"
    xcrun stapler validate "$APP_BUNDLE"
    # Gatekeeper's verdict — informational, non-fatal.
    spctl --assess --verbose=4 --type execute "$APP_BUNDLE" || true
fi

echo
echo "Built $APP_BUNDLE"
if [[ "$DO_NOTARIZE" -eq 1 ]]; then
    echo "(signed, notarized, and stapled)"
fi
du -sh "$APP_BUNDLE" | awk '{print "Size: " $1}'
