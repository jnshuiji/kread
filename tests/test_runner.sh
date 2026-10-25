#!/system/bin/sh
# ==============================================================================
# test_runner.sh - Automated Real-Device Verification Suite for kread
#
# Validates all 7 production acceptance indicators on Android GKI 6.12 (arm64):
#   Indicator 1: Zero-Page-Fault Assertion (1,000,000 reads, delta flt == 0)
#   Indicator 2: Memory Leak Detection (Slab / Kmalloc zero leak)
#   Indicator 3: Clean Unload Stress Test (100 consecutive insmod/rmmod)
#   Indicator 4: Target Process Crash Race (kill -9 during 64MB read)
#   Indicator 5: Concurrency Stress (12 threads: 4 range + 8 batch)
#   Indicator 6: Low Memory Stress (64MB read under MemAvailable < 150MB)
#   Indicator 7: Huge Page THP 2MB Data Consistency Verification
#
# Extended Integration Validations:
#   Indicator 8: Rust Daemon CLI Functionality (range, batch, bench, status)
#   Indicator 9: KernelSU action.sh Status & Diagnostics Panel
# ==============================================================================

# Support standard shells if /system/bin/sh is invoked via bash/sh
[ -z "$DIR" ] && DIR="$(cd "$(dirname "$0")" 2>/dev/null && pwd)"
[ -z "$DIR" ] && DIR="."

BIN_DIR="$DIR/bin"
TARGET_BIN="$BIN_DIR/kread_test_target"
CLIENT_BIN="$BIN_DIR/kread_test_client"
PRESSURE_BIN="$BIN_DIR/mem_pressure"

# Locate kread_daemon across candidate paths
DAEMON_BIN=""
for cand in \
    "$BIN_DIR/kread_daemon" \
    "$DIR/kread_daemon" \
    "$DIR/../daemon/target/aarch64-unknown-linux-musl/release/kread_daemon" \
    "/data/local/tmp/kread_tests/bin/kread_daemon" \
    "/data/adb/modules/kread/bin/kread_daemon"; do
    if [ -x "$cand" ]; then
        DAEMON_BIN="$cand"
        break
    fi
done

# Locate action.sh across candidate paths
ACTION_SH=""
for cand in \
    "$DIR/action.sh" \
    "$DIR/../action.sh" \
    "/data/local/tmp/kread_tests/action.sh" \
    "/data/adb/modules/kread/action.sh"; do
    if [ -f "$cand" ]; then
        ACTION_SH="$cand"
        break
    fi
done

# Locate kread.ko across multiple candidate paths
KO_PATH=""
for cand in \
    "$DIR/../kread.ko" \
    "$DIR/kread.ko" \
    "$DIR/../../../android16-6.12/bazel-bin/modules/kread/kread/kread.ko" \
    "/data/local/tmp/kread/kread.ko" \
    "/data/local/tmp/kread.ko" \
    "/data/adb/modules/kread/kread.ko"; do
    if [ -f "$cand" ]; then
        KO_PATH="$cand"
        break
    fi
done

PASS_COUNT=0
FAIL_COUNT=0

log_header() {
    echo ""
    echo "================================================================================"
    echo "  $1"
    echo "================================================================================"
}

check_result() {
    res=$1
    desc=$2
    if [ "$res" -eq 0 ]; then
        echo "  --> [PASS] $desc"
        PASS_COUNT=$((PASS_COUNT + 1))
    else
        echo "  --> [FAIL] $desc"
        FAIL_COUNT=$((FAIL_COUNT + 1))
    fi
}

echo "================================================================================"
echo "      🚀 kread Automated Real-Device Verification Suite (GKI 6.12)             "
echo "================================================================================"
echo "  Device Architecture: $(uname -m 2>/dev/null || echo 'unknown')"
echo "  Kernel Release     : $(uname -r 2>/dev/null || echo 'unknown')"
echo "  Module Candidate   : ${KO_PATH:-'NOT FOUND'}"
echo "  Daemon Candidate   : ${DAEMON_BIN:-'NOT FOUND'}"
echo "  Action Candidate   : ${ACTION_SH:-'NOT FOUND'}"
echo "  Runner Working Dir : $DIR"

# Verify test binaries exist
for b in "$TARGET_BIN" "$CLIENT_BIN" "$PRESSURE_BIN"; do
    if [ ! -x "$b" ]; then
        echo "[-] Error: Required test binary not executable: $b"
        echo "    Please run ./build_tests.sh first."
        exit 1
    fi
done

# Ensure kread.ko is loaded
if ! lsmod | grep -q "^kread "; then
    if [ -z "$KO_PATH" ]; then
        echo "[-] Error: kread.ko not loaded and binary not found!"
        exit 1
    fi
    echo "[*] Loading kread.ko from $KO_PATH..."
    insmod "$KO_PATH" || { echo "[-] insmod kread.ko failed!"; exit 1; }
fi

# Ensure /dev/kread exists
for i in 1 2 3; do
    [ -e /dev/kread ] && break
    sleep 1
done

if [ ! -e /dev/kread ]; then
    echo "[-] Error: Character device /dev/kread not found after insmod!"
    exit 1
fi
chmod 0666 /dev/kread 2>/dev/null || true
echo "  ✓ /dev/kread present: $(ls -l /dev/kread)"

# ------------------------------------------------------------------------------
# 0. Start Target Process and Parse Memory Layout
# ------------------------------------------------------------------------------
log_header "0. Initializing Test Target Process"
TMP_INFO="/data/local/tmp/kread_target.info"
[ -d "/data/local/tmp" ] || TMP_INFO="/tmp/kread_target.info"
rm -f "$TMP_INFO"

$TARGET_BIN > "$TMP_INFO" &
TARGET_PID=$!
sleep 1

if ! kill -0 "$TARGET_PID" 2>/dev/null; then
    echo "[-] Failed to start target process ($TARGET_BIN)"
    exit 1
fi

. "$TMP_INFO"
echo "  Target PID    : $TARGET_PID"
echo "  THP Address   : $THP_ADDR (Length: $THP_LEN bytes)"
echo "  Sparse Address: $SPARSE_ADDR (Length: $SPARSE_LEN bytes)"

# ------------------------------------------------------------------------------
# Indicator 1: Zero-Page-Fault Assertion (1,000,000 Reads)
# ------------------------------------------------------------------------------
log_header "Indicator 1: Zero-Page-Fault Invariant Assertion (1,000,000 Reads)"
$CLIENT_BIN zero-fault "$TARGET_PID" "$THP_ADDR"
check_result $? "Indicator 1: 1,000,000 cross-process reads strictly maintain delta minflt/majflt == 0"

# ------------------------------------------------------------------------------
# Indicator 2: Memory Leak Detection (Slab & Session Zero-Leak)
# ------------------------------------------------------------------------------
log_header "Indicator 2: Memory Leak Detection (Slab / Kmalloc Zero-Leak)"
echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true
sleep 1
SLAB_BEFORE=$(awk '/SUnreclaim/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)

# Execute 100,000 high-frequency operations over sparse memory holes
$CLIENT_BIN zero-fault "$TARGET_PID" "$SPARSE_ADDR" >/dev/null 2>&1
echo 3 > /proc/sys/vm/drop_caches 2>/dev/null || true
sleep 2

SLAB_AFTER=$(awk '/SUnreclaim/ {print $2}' /proc/meminfo 2>/dev/null || echo 0)
SLAB_DELTA=$((SLAB_AFTER - SLAB_BEFORE))
echo "  SUnreclaim: before=${SLAB_BEFORE} kB, after=${SLAB_AFTER} kB, delta=${SLAB_DELTA} kB"

# Check kmemleak if enabled
KMEMLEAK_RES=0
if [ -f "/sys/kernel/debug/kmemleak" ]; then
    echo scan > /sys/kernel/debug/kmemleak 2>/dev/null || true
    sleep 1
    if grep -iq "kread" /sys/kernel/debug/kmemleak 2>/dev/null; then
        echo "  [-] kmemleak detected unreleased kread allocations!"
        KMEMLEAK_RES=1
    fi
fi

# Allow <= 2048 kB normal OS background noise
LEAK_STATUS=0
if [ "$SLAB_DELTA" -gt 2048 ] || [ "$KMEMLEAK_RES" -ne 0 ]; then
    LEAK_STATUS=1
fi
check_result $LEAK_STATUS "Indicator 2: High-frequency read/write cycles produce 0 persistent slab/session leaks"

# ------------------------------------------------------------------------------
# Indicator 3: Clean Unload Stress Test (100 Consecutive insmod/rmmod)
# ------------------------------------------------------------------------------
log_header "Indicator 3: Clean Unload Stress Test (100 Consecutive insmod/rmmod)"
DMESG_MARK="=== KREAD_UNLOAD_TEST_START_$(date +%s) ==="
echo "$DMESG_MARK" > /dev/kmsg 2>/dev/null || true
UNLOAD_FAIL=0

echo "  -> Executing 100 insmod/rmmod cycles..."
for i in $(seq 1 100); do
    rmmod kread 2>/dev/null || true
    if ! insmod "$KO_PATH" 2>/dev/null; then
        echo "  [-] Cycle $i: insmod failed!"
        UNLOAD_FAIL=1
        break
    fi
    for t in 1 2 3 4 5; do
        [ -e /dev/kread ] && break
        usleep 5000 2>/dev/null || sleep 0.01 2>/dev/null || true
    done
    if [ ! -e /dev/kread ]; then
        echo "  [-] Cycle $i: /dev/kread missing after insmod!"
        UNLOAD_FAIL=1
        break
    fi
    if ! rmmod kread 2>/dev/null; then
        echo "  [-] Cycle $i: rmmod failed!"
        UNLOAD_FAIL=1
        break
    fi
done

# Re-insert kread for subsequent tests
insmod "$KO_PATH" 2>/dev/null || true
for i in 1 2 3 4 5; do
    [ -e /dev/kread ] && break
    usleep 5000 2>/dev/null || sleep 0.01 2>/dev/null || true
done
chmod 0666 /dev/kread 2>/dev/null || true

# Scan dmesg for kernel warnings/oops
DMESG_ERR=""
if [ -n "$DMESG_MARK" ]; then
    DMESG_ERR=$(dmesg 2>/dev/null | grep -A 99999 "$DMESG_MARK" 2>/dev/null | grep -E '(^|[[:space:]])(Warning|Oops|BUG|kernel BUG):|Call trace:' || true)
fi

if [ -n "$DMESG_ERR" ]; then
    echo "  [-] Kernel warning/error detected in dmesg:"
    echo "$DMESG_ERR" | tail -n 5
    UNLOAD_FAIL=1
else
    echo "  ✓ 100 cycles completed cleanly. Zero dmesg warnings."
fi
check_result $UNLOAD_FAIL "Indicator 3: 100 consecutive insmod/rmmod cycles execute with 0 dmesg warnings/oops"

# ------------------------------------------------------------------------------
# Indicator 4: Target Process Crash Race (kill -9 Target Mid-Read)
# ------------------------------------------------------------------------------
log_header "Indicator 4: Target Process Crash Race (kill -9 Race Handling)"
RACE_FAIL=0
echo "  -> Testing abrupt target termination during active 64 MiB reads (20 iterations)..."
for r in $(seq 1 20); do
    $TARGET_BIN > /dev/null 2>&1 &
    DUMMY_PID=$!
    sleep 0.1

    # Start asynchronous 64 MiB stream read
    $CLIENT_BIN stream-read "$DUMMY_PID" 0x100000 67108864 >/dev/null 2>&1 &
    READER_PID=$!
    usleep 3000 2>/dev/null || sleep 0.003 2>/dev/null || true

    # Kill target process abruptly mid-read
    kill -9 "$DUMMY_PID" 2>/dev/null || true

    # Wait for reader process; ensure it does not hang in D-state
    wait "$READER_PID" 2>/dev/null
    READER_STATUS=$?
    if [ "$READER_STATUS" -gt 128 ]; then
        echo "  [-] Iteration $r: Reader process hung or killed by signal ($READER_STATUS)"
        RACE_FAIL=1
        break
    fi
done

if [ "$RACE_FAIL" -eq 0 ]; then
    echo "  ✓ 20 kill -9 race cycles handled gracefully with zero lock stalls."
fi
check_result $RACE_FAIL "Indicator 4: Mid-read target crash (kill -9) handled gracefully without deadlock or D-state"

# ------------------------------------------------------------------------------
# Indicator 5: Concurrency Stress (12 Concurrent Threads)
# ------------------------------------------------------------------------------
log_header "Indicator 5: Concurrency Stress (12 Threads: 4 Range + 8 Batch)"
$CLIENT_BIN concur "$TARGET_PID" "$THP_ADDR" 15
check_result $? "Indicator 5: 12-thread concurrent range and batch reads sustained with zero deadlock"

# ------------------------------------------------------------------------------
# Indicator 6: Low Memory Stress (64 MiB Read Under Memory Pressure)
# ------------------------------------------------------------------------------
log_header "Indicator 6: Low Memory Stress (O(1) Streaming Under MemAvailable < 150MB)"
$PRESSURE_BIN 150 > /dev/null 2>&1 &
PRESSURE_PID=$!
sleep 3

# Perform 64 MiB streaming read while memory pressure is active
$CLIENT_BIN stream-read "$TARGET_PID" "$SPARSE_ADDR" 67108864
LOWMEM_RES=$?
kill -9 "$PRESSURE_PID" 2>/dev/null || true
wait "$PRESSURE_PID" 2>/dev/null || true

check_result $LOWMEM_RES "Indicator 6: 64 MiB streaming read completes under low memory without kvzalloc OOM"

# ------------------------------------------------------------------------------
# Indicator 7: Huge Page THP 2MB Data Consistency Verification
# ------------------------------------------------------------------------------
log_header "Indicator 7: THP 2MB Huge Page Data Consistency Verification"

# Refresh target process to ensure resident THP pages guaranteed in RAM after memory pressure
kill -9 "$TARGET_PID" 2>/dev/null || true
$TARGET_BIN > "$TMP_INFO" 2>&1 &
TARGET_PID=$!
sleep 1
eval $(grep -E '^(TARGET_PID|THP_ADDR|THP_LEN|SPARSE_ADDR|SPARSE_LEN)=' "$TMP_INFO")
echo "  Refreshed Target PID : $TARGET_PID"
echo "  THP Address          : $THP_ADDR (Length: $THP_LEN bytes)"

$CLIENT_BIN thp "$TARGET_PID" "$THP_ADDR" "$THP_LEN"
check_result $? "Indicator 7: 2 MiB-aligned THP memory read achieves 100% byte-for-byte consistency"

# ------------------------------------------------------------------------------
# Integration Test: Rust Daemon CLI Functionality
# ------------------------------------------------------------------------------
log_header "Integration Test: Rust Daemon CLI Verification"
DAEMON_FAIL=0
if [ -n "$DAEMON_BIN" ] && [ -x "$DAEMON_BIN" ]; then
    echo "  Testing daemon range subcommand..."
    if ! "$DAEMON_BIN" range "$TARGET_PID" "$THP_ADDR" 64; then
        echo "  [-] range subcommand failed"
        DAEMON_FAIL=1
    fi

    echo "  Testing daemon batch subcommand..."
    if ! "$DAEMON_BIN" batch "$TARGET_PID" "$THP_ADDR" "$THP_ADDR"; then
        echo "  [-] batch subcommand failed"
        DAEMON_FAIL=1
    fi

    echo "  Testing daemon bench subcommand..."
    if ! "$DAEMON_BIN" bench "$TARGET_PID" "$THP_ADDR" 1; then
        echo "  [-] bench subcommand failed"
        DAEMON_FAIL=1
    fi

    echo "  Testing daemon status subcommand..."
    if ! "$DAEMON_BIN" status; then
        echo "  [-] status subcommand failed"
        DAEMON_FAIL=1
    fi
else
    echo "  [-] Rust daemon binary not found or not executable: $DAEMON_BIN"
    DAEMON_FAIL=1
fi
check_result $DAEMON_FAIL "Rust Daemon CLI: range, batch, bench, and status commands execute successfully"

# ------------------------------------------------------------------------------
# Integration Test: KernelSU Action Script Verification
# ------------------------------------------------------------------------------
log_header "Integration Test: KernelSU Action Script (action.sh)"
ACTION_FAIL=0
if [ -n "$ACTION_SH" ] && [ -f "$ACTION_SH" ]; then
    chmod +x "$ACTION_SH" 2>/dev/null || true
    echo "  Executing action.sh..."
    if ! sh "$ACTION_SH"; then
        echo "  [-] action.sh execution returned failure"
        ACTION_FAIL=1
    fi
else
    echo "  [-] action.sh not found: $ACTION_SH"
    ACTION_FAIL=1
fi
check_result $ACTION_FAIL "KernelSU action.sh executes cleanly and outputs system diagnostic panel"

# ------------------------------------------------------------------------------
# Teardown & Summary
# ------------------------------------------------------------------------------
kill -9 "$TARGET_PID" 2>/dev/null || true
rm -f "$TMP_INFO"

log_header "Real-Device Acceptance Test Suite Summary"
echo "  Total Passed: $PASS_COUNT"
echo "  Total Failed: $FAIL_COUNT"
echo ""

if [ "$FAIL_COUNT" -eq 0 ]; then
    echo "🎉 ALL REAL-DEVICE ACCEPTANCE INDICATORS AND INTEGRATION TESTS PASSED!"
    exit 0
else
    echo "❌ TEST SUITE FAILED ($FAIL_COUNT tests failed). Review logs above."
    exit 1
fi
