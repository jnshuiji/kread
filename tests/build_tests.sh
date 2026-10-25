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
NDK_CLANG="$WORKSPACE_ROOT/android16-6.12/prebuilts/ndk-r26/toolchains/llvm/prebuilt/linux-x86_64/bin/aarch64-linux-android34-clang"

if [ ! -x "$NDK_CLANG" ]; then
    echo "[-] Error: Prebuilt NDK Clang not found or not executable at:"
    echo "    $NDK_CLANG"
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
