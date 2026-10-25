#!/system/bin/sh
# KernelSU Action script for kread Companion

MODDIR=${0%/*}
export KREAD_RUNTIME_DIR="$MODDIR"

echo "=========================================="
echo "       kread Companion Action Status      "
echo "=========================================="
echo "Timestamp: $(date -u '+%Y-%m-%dT%H:%M:%SZ')"
echo

# 1. Kernel Module Status
echo "[1] Kernel Module Status:"
if lsmod | grep -q "^kread "; then
    MODULE_LINE=$(lsmod | grep "^kread ")
    echo "  Status: LOADED"
    echo "  Details: $MODULE_LINE"
else
    echo "  Status: NOT LOADED"
    if [ -f "$MODDIR/kread.ko" ]; then
        echo "  Attempting to load module..."
        insmod "$MODDIR/kread.ko"
        if lsmod | grep -q "^kread "; then
            echo "  Status: LOADED successfully"
        else
            echo "  Error: Failed to load $MODDIR/kread.ko"
        fi
    fi
fi
echo

# 2. Character Device Status
echo "[2] Character Device (/dev/kread):"
if [ -e /dev/kread ]; then
    echo "  Presence: PRESENT"
    echo "  Permissions: $(ls -l /dev/kread)"
    # Ensure secure permissions (0600 root:root)
    chown root:root /dev/kread 2>/dev/null || true
    chmod 0600 /dev/kread 2>/dev/null || true
    echo "  Secured: $(ls -l /dev/kread)"
else
    echo "  Presence: MISSING (/dev/kread not found)"
fi
echo

# 3. Rust Daemon Status
echo "[3] Background Daemon Status:"
DAEMON_PID=$(pidof kread_daemon 2>/dev/null || pgrep kread_daemon 2>/dev/null)
if [ -n "$DAEMON_PID" ]; then
    echo "  State: RUNNING (PID: $DAEMON_PID)"
else
    echo "  State: STOPPED"
fi

# Locate kread_daemon binary to invoke status
DAEMON_BIN=""
if [ -x "$MODDIR/bin/kread_daemon" ]; then
    DAEMON_BIN="$MODDIR/bin/kread_daemon"
elif [ -x "/data/local/tmp/kread_tests/bin/kread_daemon" ]; then
    DAEMON_BIN="/data/local/tmp/kread_tests/bin/kread_daemon"
elif command -v kread_daemon >/dev/null 2>&1; then
    DAEMON_BIN=$(command -v kread_daemon)
fi

if [ -n "$DAEMON_BIN" ]; then
    echo "  Binary: $DAEMON_BIN"
    echo "  Daemon Status JSON Report:"
    "$DAEMON_BIN" status 2>&1 | sed 's/^/    /'
else
    echo "  Binary: kread_daemon binary not found in standard paths"
fi

echo
echo "=========================================="
echo "  Self-Test Complete.                     "
echo "=========================================="
