#!/usr/bin/env bash
# One-click build script for kread (Kernel Module + Rust Daemon + KernelSU Flashable Zip)
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
KLEAF_ROOT="$PROJECT_ROOT/android16-6.12"
OUT_DIR="$SCRIPT_DIR/out"
STAGING_DIR="$OUT_DIR/staging"
VERSION=$(grep -E '^version=' "$SCRIPT_DIR/module.prop" | cut -d= -f2 | tr -d '[:space:]')
[ -z "$VERSION" ] && VERSION="1"
ZIP_NAME="kread-$VERSION.zip"
ZIP_PATH="$OUT_DIR/$ZIP_NAME"

echo "=========================================================="
echo "      kread Unified Build & Packaging Engine             "
echo "=========================================================="
echo "Project Root: $PROJECT_ROOT"
echo "Module Dir:   $SCRIPT_DIR"
echo "Output Zip:   $ZIP_PATH"
echo

# 1. Compile Kernel Module with Kleaf Bazel
echo "[1/4] Building GKI 6.12 kread.ko via Kleaf Bazel..."
if [ -d "$KLEAF_ROOT" ]; then
    (
        cd "$KLEAF_ROOT"
        tools/bazel build //modules/kread:kread
    )
    KO_SOURCE="$KLEAF_ROOT/bazel-bin/modules/kread/kread/kread.ko"
    if [ -f "$KO_SOURCE" ]; then
        cp -f "$KO_SOURCE" "$SCRIPT_DIR/kread.ko"
        echo "  [OK] Copied kread.ko to $SCRIPT_DIR/kread.ko ($(stat -c%s "$SCRIPT_DIR/kread.ko") bytes)"
    else
        echo "  [ERROR] Kernel module build artifact not found at $KO_SOURCE"
        exit 1
    fi
else
    echo "  [WARN] Kleaf root $KLEAF_ROOT not found, checking local kread.ko..."
    if [ ! -f "$SCRIPT_DIR/kread.ko" ]; then
        echo "  [ERROR] No kread.ko found."
        exit 1
    fi
fi

# 2. Compile Rust Daemon for static aarch64-unknown-linux-musl
echo
echo "[2/4] Building static ARM64 Rust Daemon..."
DAEMON_DIR="$SCRIPT_DIR/daemon"
if [ -d "$DAEMON_DIR" ]; then
    (
        cd "$DAEMON_DIR"
        cargo build --release --target aarch64-unknown-linux-musl
    )
    DAEMON_SOURCE="$DAEMON_DIR/target/aarch64-unknown-linux-musl/release/kread_daemon"
    if [ -f "$DAEMON_SOURCE" ]; then
        echo "  [OK] Built static kread_daemon ($(stat -c%s "$DAEMON_SOURCE") bytes)"
    else
        echo "  [ERROR] Daemon binary not found at $DAEMON_SOURCE"
        exit 1
    fi
else
    echo "  [ERROR] Daemon directory $DAEMON_DIR does not exist."
    exit 1
fi

# 3. Assemble Staging Directory
echo
echo "[3/4] Assembling KernelSU Module Staging Area..."
rm -rf "$STAGING_DIR"
mkdir -p "$STAGING_DIR/bin"

cp -f "$SCRIPT_DIR/module.prop" "$STAGING_DIR/"
cp -f "$SCRIPT_DIR/customize.sh" "$STAGING_DIR/"
cp -f "$SCRIPT_DIR/service.sh" "$STAGING_DIR/"
cp -f "$SCRIPT_DIR/action.sh" "$STAGING_DIR/"
cp -f "$SCRIPT_DIR/kread.ko" "$STAGING_DIR/"
cp -f "$DAEMON_SOURCE" "$STAGING_DIR/bin/kread_daemon"

chmod 0755 "$STAGING_DIR/customize.sh"
chmod 0755 "$STAGING_DIR/service.sh"
chmod 0755 "$STAGING_DIR/action.sh"
chmod 0755 "$STAGING_DIR/bin/kread_daemon"
chmod 0644 "$STAGING_DIR/module.prop"
chmod 0644 "$STAGING_DIR/kread.ko"

echo "  Staging contents:"
ls -la "$STAGING_DIR"
ls -la "$STAGING_DIR/bin"

# 4. Create Flashable Zip
echo
echo "[4/4] Generating Flashable Zip ($ZIP_NAME)..."
mkdir -p "$OUT_DIR"
rm -f "$ZIP_PATH"

python3 - <<EOF
import os, sys, zipfile

staging_dir = "$STAGING_DIR"
zip_path = "$ZIP_PATH"

with zipfile.ZipFile(zip_path, 'w', zipfile.ZIP_DEFLATED) as zf:
    for root, dirs, files in os.walk(staging_dir):
        for f in sorted(files):
            full_path = os.path.join(root, f)
            rel_path = os.path.relpath(full_path, staging_dir)
            zinfo = zipfile.ZipInfo.from_file(full_path, arcname=rel_path)
            mode = os.stat(full_path).st_mode
            zinfo.external_attr = (mode & 0xFFFF) << 16
            with open(full_path, 'rb') as fp:
                zf.writestr(zinfo, fp.read(), compress_type=zipfile.ZIP_DEFLATED)
EOF

# Also create kread-ksu alias
cp -f "$ZIP_PATH" "$OUT_DIR/kread-ksu-$VERSION.zip"

echo
echo "=========================================================="
echo "  [SUCCESS] Flashable KernelSU Zip Created:"
echo "  Primary:       $ZIP_PATH ($(stat -c%s "$ZIP_PATH") bytes)"
echo "  Compatibility: $OUT_DIR/kread-ksu-$VERSION.zip"
echo "=========================================================="
