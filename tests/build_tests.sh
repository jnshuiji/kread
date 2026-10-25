#!/usr/bin/env bash
# ==============================================================================
# modules/kread/tests/build_tests.sh
#
# Cross-compiles real-device verification binaries for aarch64 Android GKI 6.12
# using the prebuilt Android NDK r26 Clang toolchain.
# ==============================================================================

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORKSPACE_ROOT="$(cd "$SCRIPT_DIR/../../.." && pwd)"

# Resolve aarch64 cross-compiler with environment variable fallback priority:
# 1. $NDK_CLANG explicit path
# 2. $ANDROID_NDK_ROOT / $ANDROID_NDK_HOME standard directories
# 3. Prebuilt GKI workspace NDK r26 path
# 4. Host aarch64-linux-gnu-gcc
if [ -z "${NDK_CLANG:-}" ]; then
    if [ -n "${ANDROID_NDK_ROOT:-}" ] && [ -x "$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang" ]; then
        NDK_CLANG="$ANDROID_NDK_ROOT/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang"
    elif [ -n "${ANDROID_NDK_HOME:-}" ] && [ -x "$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang" ]; then
        NDK_CLANG="$ANDROID_NDK_HOME/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang"
    elif [ -x "$WORKSPACE_ROOT/android16-6.12/prebuilts/ndk-r26/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang" ]; then
        NDK_CLANG="$WORKSPACE_ROOT/android16-6.12/prebuilts/ndk-r26/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang"
    elif command -v aarch64-linux-gnu-gcc >/dev/null 2>&1; then
        NDK_CLANG="aarch64-linux-gnu-gcc"
    fi
fi

if [ -z "${NDK_CLANG:-}" ] || ! command -v "$NDK_CLANG" >/dev/null 2>&1 && [ ! -x "${NDK_CLANG:-}" ]; then
    echo "[-] Error: Cross-compiler for aarch64 not found."
    echo "    Please set NDK_CLANG or ANDROID_NDK_ROOT, or install aarch64-linux-gnu-gcc."
    exit 1
fi

OUT_DIR="$SCRIPT_DIR/bin"
mkdir -p "$OUT_DIR"

echo "================================================================================"
echo "  🔨 Cross-Compiling kread Test Suite (Target: aarch64-linux-android34)"
echo "================================================================================"

echo "  -> Compiling kread_test_target..."
"$NDK_CLANG" -O2 -static -pthread "$SCRIPT_DIR/src/kread_test_target.c" -o "$OUT_DIR/kread_test_target"
chmod +x "$OUT_DIR/kread_test_target"
echo "     ✓ Generated: $OUT_DIR/kread_test_target ($(stat -c%s "$OUT_DIR/kread_test_target") bytes)"

echo "  -> Compiling kread_test_client..."
"$NDK_CLANG" -O2 -static -pthread -I"$SCRIPT_DIR/../include" "$SCRIPT_DIR/src/kread_test_client.c" -o "$OUT_DIR/kread_test_client"
chmod +x "$OUT_DIR/kread_test_client"
echo "     ✓ Generated: $OUT_DIR/kread_test_client ($(stat -c%s "$OUT_DIR/kread_test_client") bytes)"

echo "  -> Compiling mem_pressure..."
"$NDK_CLANG" -O2 -static "$SCRIPT_DIR/src/mem_pressure.c" -o "$OUT_DIR/mem_pressure"
chmod +x "$OUT_DIR/mem_pressure"
echo "     ✓ Generated: $OUT_DIR/mem_pressure ($(stat -c%s "$OUT_DIR/mem_pressure") bytes)"

echo "================================================================================"
echo "  🎉 All real-device test binaries successfully built for arm64."
echo "================================================================================"
